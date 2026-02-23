// ═══════════════════════════════════════════════════════════════════════════
// BlazeTerm シェーダー
// ═══════════════════════════════════════════════════════════════════════════
//
// GPU加速テキストレンダリング用のWGSLシェーダー
// - インスタンスレンダリングで効率的に描画
// - 背景とテキストを別パスで描画

// ユニフォーム（全インスタンス共通のデータ）
struct Uniforms {
    screen_size: vec2<f32>,  // 画面サイズ（ピクセル）
    cell_size: vec2<f32>,    // セルサイズ（ピクセル）
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(0) @binding(1)
var glyph_texture: texture_2d<f32>;

@group(0) @binding(2)
var glyph_sampler: sampler;

// 頂点シェーダーへの入力（インスタンスごと）
struct InstanceInput {
    @location(0) position: vec2<f32>,      // グリッド座標
    @location(1) fg_color: vec4<f32>,      // 前景色
    @location(2) bg_color: vec4<f32>,      // 背景色
    @location(3) uv_offset: vec2<f32>,     // UV座標オフセット
    @location(4) uv_size: vec2<f32>,       // UVサイズ
    @location(5) glyph_offset: vec2<f32>,  // グリフオフセット
    @location(6) glyph_size: vec2<f32>,    // グリフサイズ
}

// 頂点シェーダーからフラグメントシェーダーへの出力
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) fg_color: vec4<f32>,
    @location(1) bg_color: vec4<f32>,
    @location(2) uv: vec2<f32>,
}

// ───────────────────────────────────────────────────────────────────────────
// 背景用シェーダー
// ───────────────────────────────────────────────────────────────────────────

@vertex
fn vs_bg(
    @builtin(vertex_index) vertex_index: u32,
    instance: InstanceInput,
) -> VertexOutput {
    var out: VertexOutput;

    // 4頂点のクワッド（TriangleStrip）
    // 0: 左上, 1: 右上, 2: 左下, 3: 右下
    let x = f32(vertex_index & 1u);
    let y = f32((vertex_index >> 1u) & 1u);

    // ピクセル座標を計算
    let pixel_pos = (instance.position + vec2<f32>(x, y)) * uniforms.cell_size;

    // クリップ座標に変換（-1〜1の範囲）
    let clip_pos = (pixel_pos / uniforms.screen_size) * 2.0 - 1.0;

    out.clip_position = vec4<f32>(clip_pos.x, -clip_pos.y, 0.0, 1.0);
    out.fg_color = instance.fg_color;
    out.bg_color = instance.bg_color;
    out.uv = vec2<f32>(0.0, 0.0);

    return out;
}

@fragment
fn fs_bg(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.bg_color;
}

// ───────────────────────────────────────────────────────────────────────────
// テキスト用シェーダー
// ───────────────────────────────────────────────────────────────────────────

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    instance: InstanceInput,
) -> VertexOutput {
    var out: VertexOutput;

    // 4頂点のクワッド
    let x = f32(vertex_index & 1u);
    let y = f32((vertex_index >> 1u) & 1u);

    // セルの左上ピクセル座標
    let cell_pixel_pos = instance.position * uniforms.cell_size;

    // グリフをセル内に配置（ベースラインを考慮）
    // glyph_offset.x = xmin（水平オフセット）
    // glyph_offset.y = ymin（ベースラインからの距離、負の値が多い）
    let glyph_x = cell_pixel_pos.x + instance.glyph_offset.x + x * instance.glyph_size.x;

    // Y座標: セルの下端からベースラインを設定し、そこからyminを引く
    let baseline_y = cell_pixel_pos.y + uniforms.cell_size.y * 0.85;
    let glyph_y = baseline_y - instance.glyph_offset.y - instance.glyph_size.y + y * instance.glyph_size.y;

    let adjusted_pos = vec2<f32>(glyph_x, glyph_y);

    // クリップ座標に変換
    let clip_pos = (adjusted_pos / uniforms.screen_size) * 2.0 - 1.0;

    out.clip_position = vec4<f32>(clip_pos.x, -clip_pos.y, 0.0, 1.0);
    out.fg_color = instance.fg_color;
    out.bg_color = instance.bg_color;

    // UV座標を計算
    out.uv = instance.uv_offset + vec2<f32>(x, y) * instance.uv_size;

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // グリフテクスチャからアルファ値をサンプリング
    let alpha = textureSample(glyph_texture, glyph_sampler, in.uv).r;

    // 前景色にアルファを適用
    return vec4<f32>(in.fg_color.rgb, in.fg_color.a * alpha);
}
