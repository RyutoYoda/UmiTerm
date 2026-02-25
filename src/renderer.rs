//! GPU レンダラーモジュール
//!
//! wgpu を使用して GPU 加速レンダリングを実現
//!
//! 高速化のポイント:
//! - インスタンスレンダリング（1ドローコールで全セルを描画）
//! - グリフキャッシュ（フォントのラスタライズは1回だけ）
//! - ダーティ領域のみ更新

use std::collections::HashMap;
use std::fs;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use fontdue::{Font, FontSettings};
use wgpu::util::DeviceExt;

use crate::explorer::{EntryKind, Explorer};
use crate::grid::Color;
use crate::terminal::{CursorShape, Terminal};

// ═══════════════════════════════════════════════════════════════════════════
// フォント読み込み（プラットフォーム対応）
// ═══════════════════════════════════════════════════════════════════════════

/// システムフォントを読み込む
/// macOS, Linux, Windows に対応
fn load_system_font() -> Result<Font> {
    // 候補フォントパス（優先度順）
    let font_paths = [
        // macOS
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/SFMono-Regular.otf",
        "/Library/Fonts/SF-Mono-Regular.otf",
        // Linux
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        // Windows
        "C:/Windows/Fonts/consola.ttf",
        "C:/Windows/Fonts/cour.ttf",
    ];

    for path in &font_paths {
        if let Ok(data) = fs::read(path) {
            if let Ok(font) = Font::from_bytes(data, FontSettings::default()) {
                log::info!("フォントを読み込みました: {}", path);
                return Ok(font);
            }
        }
    }

    // 環境変数でカスタムフォントを指定可能
    if let Ok(custom_path) = std::env::var("UMITERM_FONT") {
        let data = fs::read(&custom_path)
            .with_context(|| format!("カスタムフォントの読み込みに失敗: {}", custom_path))?;
        return Font::from_bytes(data, FontSettings::default())
            .map_err(|e| anyhow::anyhow!("フォントのパースに失敗: {}", e));
    }

    anyhow::bail!(
        "システムフォントが見つかりません。\n\
         UMITERM_FONT 環境変数でフォントパスを指定してください。"
    )
}

/// 日本語フォールバックフォントを読み込む
fn load_japanese_font() -> Option<Font> {
    let font_paths = [
        // macOS - 日本語フォント
        "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        // Linux
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        // Windows
        "C:/Windows/Fonts/msgothic.ttc",
        "C:/Windows/Fonts/YuGothM.ttc",
    ];

    for path in &font_paths {
        if let Ok(data) = fs::read(path) {
            if let Ok(font) = Font::from_bytes(data, FontSettings::default()) {
                log::info!("日本語フォントを読み込みました: {}", path);
                return Some(font);
            }
        }
    }

    log::warn!("日本語フォールバックフォントが見つかりません");
    None
}

// ═══════════════════════════════════════════════════════════════════════════
// 定数
// ═══════════════════════════════════════════════════════════════════════════

/// デフォルトのフォントサイズ（ピクセル）
const DEFAULT_FONT_SIZE: f32 = 22.0;

/// グリフアトラスの初期サイズ（メモリ最適化: 512x512 = 256KB）
const ATLAS_SIZE: u32 = 512;

/// 最大インスタンス数（メモリ最適化、オーバーフロー保護あり）
const MAX_INSTANCES: usize = 8000;

// ═══════════════════════════════════════════════════════════════════════════
// 頂点データ（GPU に送るデータ）
// ═══════════════════════════════════════════════════════════════════════════

/// セルインスタンスデータ
/// 各セルの描画に必要な情報をGPUに送る
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CellInstance {
    /// セルの位置（グリッド座標）
    position: [f32; 2],
    /// 前景色
    fg_color: [f32; 4],
    /// 背景色
    bg_color: [f32; 4],
    /// グリフのUV座標（テクスチャ内での位置）
    uv_offset: [f32; 2],
    /// グリフのサイズ（テクスチャ内）
    uv_size: [f32; 2],
    /// グリフのオフセット（ベースラインからの調整）
    glyph_offset: [f32; 2],
    /// グリフの実際のサイズ
    glyph_size: [f32; 2],
}

// ═══════════════════════════════════════════════════════════════════════════
// グリフキャッシュ
// ═══════════════════════════════════════════════════════════════════════════

/// グリフのキャッシュ情報
#[derive(Clone)]
struct GlyphInfo {
    /// テクスチャ内のUV座標
    uv_offset: [f32; 2],
    /// テクスチャ内のサイズ
    uv_size: [f32; 2],
    /// ベースラインからのオフセット
    offset: [f32; 2],
    /// グリフの実サイズ
    size: [f32; 2],
}

/// グリフアトラス（文字のテクスチャキャッシュ）
struct GlyphAtlas {
    /// キャッシュされたグリフ
    glyphs: HashMap<char, GlyphInfo>,
    /// アトラステクスチャのピクセルデータ
    pixels: Vec<u8>,
    /// 現在の書き込み位置X
    cursor_x: u32,
    /// 現在の書き込み位置Y
    cursor_y: u32,
    /// 現在の行の最大高さ
    row_height: u32,
    /// アトラスの幅
    width: u32,
    /// アトラスの高さ
    height: u32,
    /// 更新が必要か
    dirty: bool,
}

impl GlyphAtlas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            glyphs: HashMap::new(),
            pixels: vec![0; (width * height) as usize],
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
            width,
            height,
            dirty: true,
        }
    }

    /// グリフを追加（なければラスタライズ）
    fn get_or_insert(
        &mut self,
        c: char,
        font: &Font,
        fallback_font: Option<&Font>,
        font_size: f32,
    ) -> Option<GlyphInfo> {
        // キャッシュにあればそれを返す
        if let Some(info) = self.glyphs.get(&c) {
            return Some(info.clone());
        }

        // メインフォントでラスタライズを試みる
        let (metrics, bitmap) = if font.has_glyph(c) {
            font.rasterize(c, font_size)
        } else if let Some(fb) = fallback_font {
            // フォールバックフォントを試す
            if fb.has_glyph(c) {
                fb.rasterize(c, font_size)
            } else {
                // どちらにもない場合はメインフォントで（豆腐になる）
                font.rasterize(c, font_size)
            }
        } else {
            font.rasterize(c, font_size)
        };

        if metrics.width == 0 || metrics.height == 0 {
            // 空白文字など
            let info = GlyphInfo {
                uv_offset: [0.0, 0.0],
                uv_size: [0.0, 0.0],
                offset: [0.0, 0.0],
                size: [metrics.advance_width, font_size],
            };
            self.glyphs.insert(c, info.clone());
            return Some(info);
        }

        // 配置場所を決定
        let w = metrics.width as u32;
        let h = metrics.height as u32;

        // 行に収まらなければ次の行へ
        if self.cursor_x + w > self.width {
            self.cursor_x = 0;
            self.cursor_y += self.row_height;
            self.row_height = 0;
        }

        // アトラスに収まらなければ失敗
        if self.cursor_y + h > self.height {
            log::warn!("グリフアトラスが満杯です");
            return None;
        }

        // ピクセルをコピー
        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) as usize;
                let dst_idx = ((self.cursor_y + y) * self.width + self.cursor_x + x) as usize;
                self.pixels[dst_idx] = bitmap[src_idx];
            }
        }

        let info = GlyphInfo {
            uv_offset: [
                self.cursor_x as f32 / self.width as f32,
                self.cursor_y as f32 / self.height as f32,
            ],
            uv_size: [w as f32 / self.width as f32, h as f32 / self.height as f32],
            offset: [metrics.xmin as f32, metrics.ymin as f32],
            size: [w as f32, h as f32],
        };

        self.glyphs.insert(c, info.clone());

        // カーソルを進める
        self.cursor_x += w + 1; // 1ピクセルの余白
        self.row_height = self.row_height.max(h + 1);
        self.dirty = true;

        Some(info)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// レンダラー
// ═══════════════════════════════════════════════════════════════════════════

/// GPU レンダラー
pub struct Renderer {
    /// wgpu サーフェス（内部で保持）
    surface: wgpu::Surface<'static>,
    /// wgpu デバイス
    device: wgpu::Device,
    /// コマンドキュー
    queue: wgpu::Queue,
    /// サーフェス設定
    surface_config: wgpu::SurfaceConfiguration,
    /// レンダーパイプライン
    render_pipeline: wgpu::RenderPipeline,
    /// 背景用パイプライン
    bg_pipeline: wgpu::RenderPipeline,
    /// インスタンスバッファ
    instance_buffer: wgpu::Buffer,
    /// 背景インスタンスバッファ
    bg_instance_buffer: wgpu::Buffer,
    /// グリフアトラステクスチャ
    atlas_texture: wgpu::Texture,
    /// テクスチャビュー
    #[allow(dead_code)]
    atlas_view: wgpu::TextureView,
    /// サンプラー
    #[allow(dead_code)]
    sampler: wgpu::Sampler,
    /// バインドグループ
    bind_group: wgpu::BindGroup,
    /// ユニフォームバッファ
    uniform_buffer: wgpu::Buffer,
    /// フォント
    font: Font,
    /// フォールバックフォント（日本語等）- 遅延読み込み
    fallback_font: Option<Font>,
    /// フォールバックフォント読み込み試行済みフラグ
    fallback_font_tried: bool,
    /// フォントサイズ
    font_size: f32,
    /// セル幅
    cell_width: f32,
    /// セル高さ
    cell_height: f32,
    /// グリフアトラス
    glyph_atlas: GlyphAtlas,
    /// 画面の幅
    width: u32,
    /// 画面の高さ
    height: u32,
}

/// ユニフォームデータ（シェーダーに渡す定数）
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    /// 画面サイズ
    screen_size: [f32; 2],
    /// セルサイズ
    cell_size: [f32; 2],
}

impl Renderer {
    /// 新しいレンダラーを作成
    pub async fn new(
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        adapter: &wgpu::Adapter,
    ) -> anyhow::Result<Self> {
        // デバイスとキューを取得（最新の wgpu 25 API）
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;

        // サーフェス設定
        let caps = surface.get_capabilities(adapter);
        let format = caps.formats[0];

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo, // VSync
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        // フォントをロード（システムフォントから動的に読み込み）
        let font = load_system_font()?;
        // 日本語フォールバックフォントは遅延読み込み（起動高速化）
        let fallback_font = None;
        let fallback_font_tried = false;

        let font_size = DEFAULT_FONT_SIZE;

        // セルサイズを計算
        let metrics = font.metrics('M', font_size);
        let cell_width = metrics.advance_width.ceil();
        let cell_height = font_size * 1.2;

        // グリフアトラスを作成
        let glyph_atlas = GlyphAtlas::new(ATLAS_SIZE, ATLAS_SIZE);

        // アトラステクスチャを作成
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Glyph Atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ユニフォームバッファ
        let uniforms = Uniforms {
            screen_size: [width as f32, height as f32],
            cell_size: [cell_width, cell_height],
        };

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Uniform Buffer"),
            contents: bytemuck::cast_slice(&[uniforms]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // バインドグループレイアウト
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Bind Group Layout"),
            entries: &[
                // ユニフォーム
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // テクスチャ
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                // サンプラー
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Bind Group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        // シェーダーモジュール
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // 背景用パイプライン
        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Background Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bg"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CellInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,  // position
                        1 => Float32x4,  // fg_color
                        2 => Float32x4,  // bg_color
                        3 => Float32x2,  // uv_offset
                        4 => Float32x2,  // uv_size
                        5 => Float32x2,  // glyph_offset
                        6 => Float32x2,  // glyph_size
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_bg"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // テキスト用パイプライン
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Text Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CellInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x4,
                        2 => Float32x4,
                        3 => Float32x2,
                        4 => Float32x2,
                        5 => Float32x2,
                        6 => Float32x2,
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // インスタンスバッファ（メモリ最適化: 8000セル = 約576KB × 2）
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Instance Buffer"),
            size: (MAX_INSTANCES * std::mem::size_of::<CellInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bg_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("BG Instance Buffer"),
            size: (MAX_INSTANCES * std::mem::size_of::<CellInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            surface,
            device,
            queue,
            surface_config,
            render_pipeline,
            bg_pipeline,
            instance_buffer,
            bg_instance_buffer,
            atlas_texture,
            atlas_view,
            sampler,
            bind_group,
            uniform_buffer,
            font,
            fallback_font,
            fallback_font_tried,
            font_size,
            cell_width,
            cell_height,
            glyph_atlas,
            width,
            height,
        })
    }

    /// ターミナルを描画
    pub fn render(&mut self, terminal: &Terminal) -> Result<(), wgpu::SurfaceError> {
        // インスタンスデータを構築
        let (instances, bg_instances) = self.build_instances(terminal);

        // グリフアトラスを更新（wgpu 25 の新しい型名を使用）
        if self.glyph_atlas.dirty {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.glyph_atlas.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.glyph_atlas.width),
                    rows_per_image: Some(self.glyph_atlas.height),
                },
                wgpu::Extent3d {
                    width: self.glyph_atlas.width,
                    height: self.glyph_atlas.height,
                    depth_or_array_layers: 1,
                },
            );
            self.glyph_atlas.dirty = false;
        }

        // インスタンスバッファを更新（オーバーフロー防止）
        let instances = if instances.len() > MAX_INSTANCES {
            &instances[..MAX_INSTANCES]
        } else {
            &instances[..]
        };
        let bg_instances = if bg_instances.len() > MAX_INSTANCES {
            &bg_instances[..MAX_INSTANCES]
        } else {
            &bg_instances[..]
        };
        self.queue
            .write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        self.queue
            .write_buffer(&self.bg_instance_buffer, 0, bytemuck::cast_slice(bg_instances));

        // 描画（内部のサーフェスを使用）
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // 背景を描画
            render_pass.set_pipeline(&self.bg_pipeline);
            render_pass.set_bind_group(0, &self.bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.bg_instance_buffer.slice(..));
            render_pass.draw(0..4, 0..bg_instances.len() as u32);

            // テキストを描画
            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
            render_pass.draw(0..4, 0..instances.len() as u32);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    /// エクスプローラーオーバーレイを描画（中央ポップアップ）
    pub fn render_explorer_overlay(
        &mut self,
        explorer: &Explorer,
        screen_cols: usize,
        screen_rows: usize,
    ) -> (Vec<CellInstance>, Vec<CellInstance>) {
        let mut instances = Vec::new();
        let mut bg_instances = Vec::new();

        // ポップアップのサイズと位置
        let popup_width = 50.min(screen_cols.saturating_sub(4));
        let popup_height = 20.min(screen_rows.saturating_sub(4));
        let start_col = (screen_cols.saturating_sub(popup_width)) / 2;
        let start_row = (screen_rows.saturating_sub(popup_height)) / 2;

        // 背景（半透明風の暗い色）
        let bg_color = Color::rgb(25, 30, 40).to_f32_array();
        let header_bg = Color::rgb(40, 50, 65).to_f32_array();
        let selected_bg = Color::rgb(180, 60, 60).to_f32_array();  // 赤で選択行を強調
        let border_color = Color::EMERALD.to_f32_array();

        // ヘッダー背景
        let header = " EXPLORER (↑↓:move Enter:open g:cd Esc:close)";
        for col in 0..popup_width {
            bg_instances.push(CellInstance {
                position: [(start_col + col) as f32, start_row as f32],
                fg_color: [0.0, 0.0, 0.0, 0.0],
                bg_color: header_bg,
                uv_offset: [0.0, 0.0],
                uv_size: [0.0, 0.0],
                glyph_offset: [0.0, 0.0],
                glyph_size: [0.0, 0.0],
            });
        }
        // ヘッダーテキスト
        for (i, c) in header.chars().enumerate() {
            if i >= popup_width { break; }
            if c != ' ' {
                self.ensure_fallback_font(c);
                if let Some(glyph) = self.glyph_atlas.get_or_insert(
                    c,
                    &self.font,
                    self.fallback_font.as_ref(),
                    self.font_size,
                ) {
                    instances.push(CellInstance {
                        position: [(start_col + i) as f32, start_row as f32],
                        fg_color: border_color,
                        bg_color: [0.0, 0.0, 0.0, 0.0],
                        uv_offset: glyph.uv_offset,
                        uv_size: glyph.uv_size,
                        glyph_offset: glyph.offset,
                        glyph_size: glyph.size,
                    });
                }
            }
        }

        // エントリを描画
        let visible_rows = popup_height - 1;
        let start = explorer.scroll_offset;
        let end = (start + visible_rows).min(explorer.entries.len());

        for (idx, entry) in explorer.entries[start..end].iter().enumerate() {
            let row = start_row + idx + 1;
            let is_selected = start + idx == explorer.selected;

            // 背景色
            let row_bg = if is_selected { selected_bg } else { bg_color };

            // アイコンとファイル名
            let indent = "  ".repeat(entry.depth);
            let icon = match entry.kind {
                EntryKind::Directory => if entry.expanded { "▼ " } else { "▶ " },
                EntryKind::File => "  ",
            };
            let display = format!(" {}{}{}", indent, icon, entry.name);

            let fg_color = match entry.kind {
                EntryKind::Directory => Color::EMERALD.to_f32_array(),
                EntryKind::File => [0.85, 0.85, 0.85, 1.0],
            };

            // 背景を先に描画（bg_instancesに追加）
            for col in 0..popup_width {
                let position = [(start_col + col) as f32, row as f32];
                bg_instances.push(CellInstance {
                    position,
                    fg_color: [0.0, 0.0, 0.0, 0.0],
                    bg_color: row_bg,
                    uv_offset: [0.0, 0.0],
                    uv_size: [0.0, 0.0],
                    glyph_offset: [0.0, 0.0],
                    glyph_size: [0.0, 0.0],
                });
            }

            // テキストを描画
            for (col, c) in display.chars().enumerate() {
                if col >= popup_width { break; }
                if c != ' ' {
                    self.ensure_fallback_font(c);
                    if let Some(glyph) = self.glyph_atlas.get_or_insert(
                        c,
                        &self.font,
                        self.fallback_font.as_ref(),
                        self.font_size,
                    ) {
                        instances.push(CellInstance {
                            position: [(start_col + col) as f32, row as f32],
                            fg_color,
                            bg_color: [0.0, 0.0, 0.0, 0.0],
                            uv_offset: glyph.uv_offset,
                            uv_size: glyph.uv_size,
                            glyph_offset: glyph.offset,
                            glyph_size: glyph.size,
                        });
                    }
                }
            }
        }

        // 残りの行を背景で埋める
        for idx in (end - start)..visible_rows {
            let row = start_row + idx + 1;
            for col in 0..popup_width {
                bg_instances.push(CellInstance {
                    position: [(start_col + col) as f32, row as f32],
                    fg_color: [0.0, 0.0, 0.0, 0.0],
                    bg_color: bg_color,
                    uv_offset: [0.0, 0.0],
                    uv_size: [0.0, 0.0],
                    glyph_offset: [0.0, 0.0],
                    glyph_size: [0.0, 0.0],
                });
            }
        }

        (instances, bg_instances)
    }

    /// 日本語フォントを遅延読み込み（必要な時のみ）
    fn ensure_fallback_font(&mut self, c: char) {
        // ASCII文字はフォールバック不要
        if c.is_ascii() {
            return;
        }
        // メインフォントにあればフォールバック不要
        if self.font.has_glyph(c) {
            return;
        }
        // 既に読み込み試行済みならスキップ
        if self.fallback_font_tried {
            return;
        }
        // 日本語フォントを読み込み
        self.fallback_font_tried = true;
        self.fallback_font = load_japanese_font();
    }

    /// グリッドからインスタンスデータを構築
    fn build_instances(&mut self, terminal: &Terminal) -> (Vec<CellInstance>, Vec<CellInstance>) {
        let grid = terminal.active_grid();
        let mut instances = Vec::with_capacity(grid.cols * grid.rows);
        let mut bg_instances = Vec::with_capacity(grid.cols * grid.rows);

        for row in 0..grid.rows {
            for col in 0..grid.cols {
                let cell = &grid[(col, row)];

                let position = [col as f32, row as f32];

                // 背景インスタンス
                bg_instances.push(CellInstance {
                    position,
                    fg_color: cell.fg.to_f32_array(),
                    bg_color: cell.bg.to_f32_array(),
                    uv_offset: [0.0, 0.0],
                    uv_size: [0.0, 0.0],
                    glyph_offset: [0.0, 0.0],
                    glyph_size: [0.0, 0.0],
                });

                // 空白以外はグリフを描画
                if cell.character != ' ' {
                    // 必要に応じて日本語フォントを遅延読み込み
                    self.ensure_fallback_font(cell.character);
                    if let Some(glyph) = self.glyph_atlas.get_or_insert(
                        cell.character,
                        &self.font,
                        self.fallback_font.as_ref(),
                        self.font_size,
                    ) {
                        instances.push(CellInstance {
                            position,
                            fg_color: cell.fg.to_f32_array(),
                            bg_color: cell.bg.to_f32_array(),
                            uv_offset: glyph.uv_offset,
                            uv_size: glyph.uv_size,
                            glyph_offset: glyph.offset,
                            glyph_size: glyph.size,
                        });
                    }
                }
            }
        }

        // カーソルを追加
        if terminal.cursor.visible {
            let cursor_char = match terminal.cursor.shape {
                CursorShape::Block => '█',
                CursorShape::Underline => '_',
                CursorShape::Beam => '│',
            };

            self.ensure_fallback_font(cursor_char);
            if let Some(glyph) = self.glyph_atlas.get_or_insert(
                cursor_char,
                &self.font,
                self.fallback_font.as_ref(),
                self.font_size,
            ) {
                instances.push(CellInstance {
                    position: [terminal.cursor.col as f32, terminal.cursor.row as f32],
                    fg_color: Color::EMERALD.to_f32_array(),
                    bg_color: [0.0, 0.0, 0.0, 0.0],
                    uv_offset: glyph.uv_offset,
                    uv_size: glyph.uv_size,
                    glyph_offset: glyph.offset,
                    glyph_size: glyph.size,
                });
            }
        }

        (instances, bg_instances)
    }

    /// サイズを変更
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);

        // ユニフォームを更新
        let uniforms = Uniforms {
            screen_size: [width as f32, height as f32],
            cell_size: [self.cell_width, self.cell_height],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));
    }

    /// ターミナルサイズを計算
    pub fn calculate_terminal_size(&self) -> (u16, u16) {
        let cols = (self.width as f32 / self.cell_width).floor() as u16;
        let rows = (self.height as f32 / self.cell_height).floor() as u16;
        (cols.max(1), rows.max(1))
    }

    /// 指定したビューポートでのターミナルサイズを計算
    pub fn calculate_terminal_size_for_viewport(&self, viewport_width: f32, viewport_height: f32) -> (u16, u16) {
        let cols = (viewport_width / self.cell_width).floor() as u16;
        let rows = (viewport_height / self.cell_height).floor() as u16;
        (cols.max(1), rows.max(1))
    }

    /// セルサイズを取得（IMEカーソル位置計算用）
    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_width, self.cell_height)
    }

    /// 画面サイズを取得
    pub fn screen_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 複数のペインを描画
    pub fn render_panes(&mut self, panes: &[(&crate::terminal::Terminal, crate::pane::Rect, bool)]) -> Result<(), wgpu::SurfaceError> {
        self.render_panes_with_explorer(panes, None)
    }

    pub fn render_panes_with_explorer(
        &mut self,
        panes: &[(&crate::terminal::Terminal, crate::pane::Rect, bool)],
        explorer: Option<&Explorer>,
    ) -> Result<(), wgpu::SurfaceError> {
        let mut all_instances = Vec::new();
        let mut all_bg_instances = Vec::new();
        let mut border_instances = Vec::new();

        // 各ペインのインスタンスデータを構築
        for (terminal, rect, is_focused) in panes {
            let (instances, bg_instances) = self.build_instances_with_viewport(terminal, rect, *is_focused);
            all_instances.extend(instances);
            all_bg_instances.extend(bg_instances);
        }

        // ペイン境界線を別に収集（後で上書き描画するため）
        if panes.len() > 1 {
            self.add_pane_borders(panes, &mut border_instances);
        }

        // エクスプローラー用の別バッファ（後から別ドローコールで描画）
        let mut explorer_instances = Vec::new();
        let mut explorer_bg_instances = Vec::new();

        // エクスプローラーオーバーレイを構築
        if let Some(exp) = explorer {
            if exp.visible {
                // 画面サイズを取得（最初のペインのターミナルから）
                let (screen_cols, screen_rows) = if let Some((terminal, _, _)) = panes.first() {
                    let grid = terminal.active_grid();
                    (grid.cols, grid.rows)
                } else {
                    (80, 24)
                };
                let (exp_instances, exp_bg) = self.render_explorer_overlay(exp, screen_cols, screen_rows);
                explorer_bg_instances = exp_bg;
                explorer_instances = exp_instances;
            }
        }

        // グリフアトラスを更新
        if self.glyph_atlas.dirty {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.glyph_atlas.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.glyph_atlas.width),
                    rows_per_image: Some(self.glyph_atlas.height),
                },
                wgpu::Extent3d {
                    width: self.glyph_atlas.width,
                    height: self.glyph_atlas.height,
                    depth_or_array_layers: 1,
                },
            );
            self.glyph_atlas.dirty = false;
        }

        // インスタンスバッファを更新（オーバーフロー防止）
        let all_instances = if all_instances.len() > MAX_INSTANCES {
            &all_instances[..MAX_INSTANCES]
        } else {
            &all_instances[..]
        };
        let all_bg_instances = if all_bg_instances.len() > MAX_INSTANCES {
            &all_bg_instances[..MAX_INSTANCES]
        } else {
            &all_bg_instances[..]
        };
        self.queue
            .write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(all_instances));
        self.queue
            .write_buffer(&self.bg_instance_buffer, 0, bytemuck::cast_slice(all_bg_instances));

        // 描画
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // 1. ターミナル背景を描画
            render_pass.set_pipeline(&self.bg_pipeline);
            render_pass.set_bind_group(0, &self.bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.bg_instance_buffer.slice(..));
            render_pass.draw(0..4, 0..all_bg_instances.len() as u32);

            // 2. ターミナルテキストを描画
            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
            render_pass.draw(0..4, 0..all_instances.len() as u32);
        }

        // 3. ペイン境界線を別パスで上に描画
        if !border_instances.is_empty() {
            let borders = if border_instances.len() > MAX_INSTANCES {
                &border_instances[..MAX_INSTANCES]
            } else {
                &border_instances[..]
            };
            self.queue
                .write_buffer(&self.bg_instance_buffer, 0, bytemuck::cast_slice(borders));

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Border Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.bg_pipeline);
            render_pass.set_bind_group(0, &self.bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.bg_instance_buffer.slice(..));
            render_pass.draw(0..4, 0..borders.len() as u32);
        }

        // 4. エクスプローラーを別のドローコールで上に描画
        if !explorer_bg_instances.is_empty() {
            // エクスプローラー用のバッファを更新
            let explorer_bg = if explorer_bg_instances.len() > MAX_INSTANCES {
                &explorer_bg_instances[..MAX_INSTANCES]
            } else {
                &explorer_bg_instances[..]
            };
            let explorer_text = if explorer_instances.len() > MAX_INSTANCES {
                &explorer_instances[..MAX_INSTANCES]
            } else {
                &explorer_instances[..]
            };
            self.queue
                .write_buffer(&self.bg_instance_buffer, 0, bytemuck::cast_slice(explorer_bg));
            self.queue
                .write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(explorer_text));

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Explorer Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,  // 既存の描画を保持
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // エクスプローラー背景
            render_pass.set_pipeline(&self.bg_pipeline);
            render_pass.set_bind_group(0, &self.bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.bg_instance_buffer.slice(..));
            render_pass.draw(0..4, 0..explorer_bg.len() as u32);

            // エクスプローラーテキスト
            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
            render_pass.draw(0..4, 0..explorer_text.len() as u32);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    /// ビューポート付きでインスタンスデータを構築
    fn build_instances_with_viewport(
        &mut self,
        terminal: &Terminal,
        viewport: &crate::pane::Rect,
        is_focused: bool,
    ) -> (Vec<CellInstance>, Vec<CellInstance>) {
        let grid = terminal.active_grid();
        let mut instances = Vec::with_capacity(grid.cols * grid.rows);
        let mut bg_instances = Vec::with_capacity(grid.cols * grid.rows);

        // ビューポートのピクセル座標を計算
        let vp_x = viewport.x * self.width as f32;
        let vp_y = viewport.y * self.height as f32;

        // セル座標へのオフセット
        let col_offset = vp_x / self.cell_width;
        let row_offset = vp_y / self.cell_height;

        // 選択ハイライト色（明るい水色背景）
        let selection_bg = [0.2, 0.5, 0.7, 1.0]; // 選択範囲の背景色
        let selection_fg = [1.0, 1.0, 1.0, 1.0]; // 選択範囲の前景色

        for row in 0..grid.rows {
            for col in 0..grid.cols {
                let cell = &grid[(col, row)];
                let is_selected = terminal.selection.contains(col, row);

                let position = [col as f32 + col_offset, row as f32 + row_offset];

                // 選択されているセルは背景色を変更
                let (fg, bg) = if is_selected {
                    (selection_fg, selection_bg)
                } else {
                    (cell.fg.to_f32_array(), cell.bg.to_f32_array())
                };

                // 背景インスタンス
                bg_instances.push(CellInstance {
                    position,
                    fg_color: fg,
                    bg_color: bg,
                    uv_offset: [0.0, 0.0],
                    uv_size: [0.0, 0.0],
                    glyph_offset: [0.0, 0.0],
                    glyph_size: [0.0, 0.0],
                });

                // 空白以外はグリフを描画
                if cell.character != ' ' {
                    // 必要に応じて日本語フォントを遅延読み込み
                    self.ensure_fallback_font(cell.character);
                    if let Some(glyph) = self.glyph_atlas.get_or_insert(
                        cell.character,
                        &self.font,
                        self.fallback_font.as_ref(),
                        self.font_size,
                    ) {
                        instances.push(CellInstance {
                            position,
                            fg_color: fg,
                            bg_color: bg,
                            uv_offset: glyph.uv_offset,
                            uv_size: glyph.uv_size,
                            glyph_offset: glyph.offset,
                            glyph_size: glyph.size,
                        });
                    }
                }
            }
        }

        // カーソルを追加（フォーカスがあるペインのみ）
        if is_focused && terminal.cursor.visible {
            let cursor_char = match terminal.cursor.shape {
                CursorShape::Block => '█',
                CursorShape::Underline => '_',
                CursorShape::Beam => '│',
            };

            self.ensure_fallback_font(cursor_char);
            if let Some(glyph) = self.glyph_atlas.get_or_insert(
                cursor_char,
                &self.font,
                self.fallback_font.as_ref(),
                self.font_size,
            ) {
                instances.push(CellInstance {
                    position: [
                        terminal.cursor.col as f32 + col_offset,
                        terminal.cursor.row as f32 + row_offset,
                    ],
                    fg_color: Color::EMERALD.to_f32_array(),
                    bg_color: [0.0, 0.0, 0.0, 0.0],
                    uv_offset: glyph.uv_offset,
                    uv_size: glyph.uv_size,
                    glyph_offset: glyph.offset,
                    glyph_size: glyph.size,
                });
            }
        }

        (instances, bg_instances)
    }

    /// ペイン境界線を追加
    fn add_pane_borders(
        &self,
        panes: &[(&crate::terminal::Terminal, crate::pane::Rect, bool)],
        bg_instances: &mut Vec<CellInstance>,
    ) {
        let border_color = Color::rgb(80, 220, 200).to_f32_array(); // 明るい水色

        for (_terminal, rect, _is_focused) in panes {
            // 右端に境界線を描画（最右端でない場合）
            if rect.x + rect.width < 0.99 {
                let border_col = ((rect.x + rect.width) * self.width as f32 / self.cell_width) as usize;
                let start_row = (rect.y * self.height as f32 / self.cell_height) as usize;
                let end_row = ((rect.y + rect.height) * self.height as f32 / self.cell_height) as usize;

                for row in start_row..end_row {
                    bg_instances.push(CellInstance {
                        position: [border_col as f32, row as f32],
                        fg_color: border_color,
                        bg_color: border_color,
                        uv_offset: [0.0, 0.0],
                        uv_size: [0.0, 0.0],
                        glyph_offset: [0.0, 0.0],
                        glyph_size: [self.cell_width, self.cell_height], // フルセルサイズ
                    });
                }
            }

            // 下端に境界線を描画（最下端でない場合）
            if rect.y + rect.height < 0.99 {
                let border_row = ((rect.y + rect.height) * self.height as f32 / self.cell_height) as usize;
                let start_col = (rect.x * self.width as f32 / self.cell_width) as usize;
                let end_col = ((rect.x + rect.width) * self.width as f32 / self.cell_width) as usize;

                for col in start_col..end_col {
                    bg_instances.push(CellInstance {
                        position: [col as f32, border_row as f32],
                        fg_color: border_color,
                        bg_color: border_color,
                        uv_offset: [0.0, 0.0],
                        uv_size: [0.0, 0.0],
                        glyph_offset: [0.0, 0.0],
                        glyph_size: [self.cell_width, self.cell_height], // フルセルサイズ
                    });
                }
            }
        }
    }
}
