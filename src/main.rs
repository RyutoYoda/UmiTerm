//! UmiTerm - 水色テーマの高速ターミナルエミュレータ
//!
//! # アーキテクチャ（マルチウィンドウ対応）
//!
//! ```
//! ┌─────────────────────────────────────────────────────────────┐
//! │                         UmiTerm                             │
//! ├─────────────────────────────────────────────────────────────┤
//! │  App                                                        │
//! │  └─ windows: HashMap<WindowId, WindowState>                 │
//! │       └─ WindowState                                        │
//! │            ├─ window: Arc<Window>                           │
//! │            ├─ renderer: Renderer                            │
//! │            ├─ terminal: Terminal                            │
//! │            ├─ parser: AnsiParser                            │
//! │            └─ pty: Pty                                      │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # 高速化のポイント
//!
//! 1. **GPU レンダリング**: wgpu によるハードウェアアクセラレーション
//! 2. **インスタンスレンダリング**: 1ドローコールで全セルを描画
//! 3. **ゼロコピーI/O**: チャネルベースの非同期PTY通信
//! 4. **差分更新**: ダーティフラグで変更箇所のみ更新
//! 5. **グリフキャッシュ**: フォントラスタライズは1回だけ
//!
//! # キーバインド
//!
//! - `Cmd+N`: 新規ウィンドウを開く
//! - `Cmd+W`: 現在のウィンドウを閉じる

mod grid;
mod pane;
mod parser;
mod pty;
mod renderer;
mod terminal;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, KeyEvent, Modifiers, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

use crate::pane::{BorderHit, Pane, PaneId, PaneLayout, Rect};
use crate::renderer::Renderer;
use crate::terminal::Terminal;

// ═══════════════════════════════════════════════════════════════════════════
// 定数
// ═══════════════════════════════════════════════════════════════════════════

/// 最小フレーム間隔（60FPS = 約16ms）
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// 初期ウィンドウサイズ
const INITIAL_WIDTH: u32 = 1024;
const INITIAL_HEIGHT: u32 = 768;

/// 起動バナー（水色テーマ）
const STARTUP_BANNER: &str = concat!(
    "\x1b[38;2;80;220;200m",  // エメラルドブルー
    "\r\n",
    "  ██╗   ██╗███╗   ███╗██╗████████╗███████╗██████╗ ███╗   ███╗\r\n",
    "  ██║   ██║████╗ ████║██║╚══██╔══╝██╔════╝██╔══██╗████╗ ████║\r\n",
    "  ██║   ██║██╔████╔██║██║   ██║   █████╗  ██████╔╝██╔████╔██║\r\n",
    "  ██║   ██║██║╚██╔╝██║██║   ██║   ██╔══╝  ██╔══██╗██║╚██╔╝██║\r\n",
    "  ╚██████╔╝██║ ╚═╝ ██║██║   ██║   ███████╗██║  ██║██║ ╚═╝ ██║\r\n",
    "   ╚═════╝ ╚═╝     ╚═╝╚═╝   ╚═╝   ╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝\r\n",
    "\x1b[38;2;60;180;170m",  // 少し暗めのシアン
    "  ─────────────────────────────────────────────────────────────\r\n",
    "\x1b[38;2;100;200;190m", // 明るいシアン
    "  ≋ GPU-Accelerated Terminal Emulator                     v0.2\r\n",
    "\x1b[38;2;60;180;170m",
    "  ─────────────────────────────────────────────────────────────\r\n",
    "\x1b[0m",
    "\r\n",
);

// ═══════════════════════════════════════════════════════════════════════════
// アプリケーション状態
// ═══════════════════════════════════════════════════════════════════════════

/// 個々のウィンドウの状態
struct WindowState {
    /// ウィンドウ
    window: Arc<Window>,
    /// GPU レンダラー
    renderer: Renderer,
    /// ペイン群（PaneIdで管理）
    panes: std::collections::HashMap<PaneId, Pane>,
    /// ペインレイアウト
    layout: PaneLayout,
    /// フォーカス中のペインID
    focused_pane: PaneId,
    /// 最後のフレーム時刻
    last_frame: Instant,
    /// IME入力中フラグ
    ime_active: bool,
    /// 修飾キーの状態
    modifiers: Modifiers,
    /// マウス位置（正規化座標 0.0-1.0）
    mouse_pos: (f32, f32),
    /// ドラッグ中の境界線情報
    dragging_border: Option<BorderHit>,
}

/// 境界線判定の閾値（正規化座標）
const BORDER_THRESHOLD: f32 = 0.01;

/// アプリケーション全体の状態
struct App {
    /// ウィンドウ群（WindowIdで管理）
    windows: HashMap<WindowId, WindowState>,
    /// wgpu インスタンス（ウィンドウ間で共有）
    instance: wgpu::Instance,
    /// wgpu アダプター（ウィンドウ間で共有）
    adapter: Option<wgpu::Adapter>,
    /// 終了フラグ
    should_exit: bool,
}

impl WindowState {
    /// 起動バナーを表示
    fn show_startup_banner(pane: &mut Pane) {
        let mut terminal = pane.terminal.lock();
        pane.parser.process(&mut terminal, STARTUP_BANNER.as_bytes());
    }

    /// フレームを更新
    fn update(&mut self) {
        // すべてのペインを更新
        for pane in self.panes.values_mut() {
            pane.update();
        }
    }

    /// 描画
    fn render(&mut self) -> bool {
        // フレームレート制限
        let now = Instant::now();
        if now - self.last_frame < MIN_FRAME_INTERVAL {
            return true;
        }
        self.last_frame = now;

        // ペインの矩形領域を計算
        let rects = self.layout.calculate_rects(Rect::full());

        // 描画用のデータを構築
        let render_data: Vec<_> = rects
            .iter()
            .filter_map(|(pane_id, rect)| {
                self.panes.get(pane_id).map(|pane| {
                    let is_focused = *pane_id == self.focused_pane;
                    (pane, *rect, is_focused)
                })
            })
            .collect();

        // ターミナルをロックして描画
        let terminals: Vec<_> = render_data
            .iter()
            .map(|(pane, rect, is_focused)| {
                let terminal = pane.terminal.lock();
                (terminal, *rect, *is_focused)
            })
            .collect();

        // 参照のベクターを作成
        let terminal_refs: Vec<(&Terminal, Rect, bool)> = terminals
            .iter()
            .map(|(t, r, f)| (&**t, *r, *f))
            .collect();

        match self.renderer.render_panes(&terminal_refs) {
            Ok(_) => true,
            Err(wgpu::SurfaceError::Lost) => {
                let size = self.window.inner_size();
                self.renderer.resize(size.width, size.height);
                true
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                log::error!("GPUメモリ不足");
                false
            }
            Err(e) => {
                log::warn!("描画エラー: {:?}", e);
                true
            }
        }
    }

    /// 縦分割（左右に分割）
    fn split_horizontal(&mut self) -> anyhow::Result<()> {
        let (screen_width, screen_height) = self.renderer.screen_size();
        let rects = self.layout.calculate_rects(Rect::full());

        // フォーカス中のペインのサイズを取得
        let focused_rect = rects
            .iter()
            .find(|(id, _)| *id == self.focused_pane)
            .map(|(_, r)| *r)
            .unwrap_or(Rect::full());

        // 新しいペインのサイズを計算（分割後の右半分）
        let new_width = focused_rect.width / 2.0 * screen_width as f32;
        let new_height = focused_rect.height * screen_height as f32;
        let (cols, rows) = self.renderer.calculate_terminal_size_for_viewport(new_width, new_height);

        // 新しいペインを作成
        let mut new_pane = Pane::new(cols, rows)?;
        let new_id = new_pane.id;
        Self::show_startup_banner(&mut new_pane);

        // 既存のペインもリサイズ
        if let Some(pane) = self.panes.get_mut(&self.focused_pane) {
            pane.resize(cols, rows);
        }

        // レイアウトを更新
        self.layout.split_horizontal(self.focused_pane, new_id);
        self.panes.insert(new_id, new_pane);

        log::info!("縦分割: {:?} -> {:?}", self.focused_pane, new_id);
        Ok(())
    }

    /// 横分割（上下に分割）
    fn split_vertical(&mut self) -> anyhow::Result<()> {
        let (screen_width, screen_height) = self.renderer.screen_size();
        let rects = self.layout.calculate_rects(Rect::full());

        // フォーカス中のペインのサイズを取得
        let focused_rect = rects
            .iter()
            .find(|(id, _)| *id == self.focused_pane)
            .map(|(_, r)| *r)
            .unwrap_or(Rect::full());

        // 新しいペインのサイズを計算（分割後の下半分）
        let new_width = focused_rect.width * screen_width as f32;
        let new_height = focused_rect.height / 2.0 * screen_height as f32;
        let (cols, rows) = self.renderer.calculate_terminal_size_for_viewport(new_width, new_height);

        // 新しいペインを作成
        let mut new_pane = Pane::new(cols, rows)?;
        let new_id = new_pane.id;
        Self::show_startup_banner(&mut new_pane);

        // 既存のペインもリサイズ
        if let Some(pane) = self.panes.get_mut(&self.focused_pane) {
            pane.resize(cols, rows);
        }

        // レイアウトを更新
        self.layout.split_vertical(self.focused_pane, new_id);
        self.panes.insert(new_id, new_pane);

        log::info!("横分割: {:?} -> {:?}", self.focused_pane, new_id);
        Ok(())
    }

    /// 現在のペインを閉じる
    fn close_pane(&mut self) -> bool {
        // ペインが1つしかない場合はウィンドウを閉じる
        if self.panes.len() <= 1 {
            return true; // ウィンドウを閉じる
        }

        // 次のフォーカス先を決定
        let next_focus = self.layout.next_pane(self.focused_pane);

        // レイアウトからペインを削除
        if let Some(new_layout) = self.layout.remove_pane(self.focused_pane) {
            self.layout = new_layout;
        }

        // ペインを削除
        self.panes.remove(&self.focused_pane);

        // フォーカスを移動
        if let Some(next) = next_focus {
            self.focused_pane = next;
        } else if let Some(id) = self.panes.keys().next().copied() {
            self.focused_pane = id;
        }

        log::info!("ペインを閉じました。残り: {}", self.panes.len());
        false // ウィンドウは閉じない
    }

    /// 次のペインにフォーカス
    fn focus_next_pane(&mut self) {
        if let Some(next) = self.layout.next_pane(self.focused_pane) {
            self.focused_pane = next;
            log::info!("フォーカス移動: {:?}", self.focused_pane);
        }
    }

    /// 前のペインにフォーカス
    fn focus_prev_pane(&mut self) {
        if let Some(prev) = self.layout.prev_pane(self.focused_pane) {
            self.focused_pane = prev;
            log::info!("フォーカス移動: {:?}", self.focused_pane);
        }
    }

    /// キー入力を処理
    fn handle_key(&mut self, event: &KeyEvent) -> WindowCommand {
        if event.state != ElementState::Pressed {
            return WindowCommand::None;
        }

        // IME入力中はキーイベントをスキップ（ただし特殊キーは通す）
        if self.ime_active {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) |
                Key::Named(NamedKey::Enter) |
                Key::Named(NamedKey::Backspace) => {
                    // これらは通す
                }
                _ => return WindowCommand::None,
            }
        }

        let ctrl = self.modifiers.state().control_key();
        let super_key = self.modifiers.state().super_key();
        let shift = self.modifiers.state().shift_key();

        // macOSのCmd+キーを処理
        if super_key {
            if let Key::Character(c) = &event.logical_key {
                match c.as_str() {
                    "n" => return WindowCommand::NewWindow,
                    "d" if shift => return WindowCommand::SplitVertical,   // Cmd+Shift+D: 横分割
                    "d" => return WindowCommand::SplitHorizontal,          // Cmd+D: 縦分割
                    "w" => return WindowCommand::ClosePane,                // Cmd+W: ペインを閉じる
                    "]" => return WindowCommand::FocusNextPane,            // Cmd+]: 次のペイン
                    "[" => return WindowCommand::FocusPrevPane,            // Cmd+[: 前のペイン
                    _ => {}
                }
            }
        }

        // キーをバイト列に変換してPTYに送信
        let bytes: Option<Vec<u8>> = match &event.logical_key {
            // 名前付きキー
            Key::Named(named) => match named {
                NamedKey::Space => Some(b" ".to_vec()),
                NamedKey::Enter => Some(b"\r".to_vec()),
                NamedKey::Backspace => Some(b"\x7f".to_vec()),
                NamedKey::Tab => Some(b"\t".to_vec()),
                NamedKey::Escape => Some(b"\x1b".to_vec()),
                NamedKey::ArrowUp => Some(b"\x1b[A".to_vec()),
                NamedKey::ArrowDown => Some(b"\x1b[B".to_vec()),
                NamedKey::ArrowRight => Some(b"\x1b[C".to_vec()),
                NamedKey::ArrowLeft => Some(b"\x1b[D".to_vec()),
                NamedKey::Home => Some(b"\x1b[H".to_vec()),
                NamedKey::End => Some(b"\x1b[F".to_vec()),
                NamedKey::PageUp => Some(b"\x1b[5~".to_vec()),
                NamedKey::PageDown => Some(b"\x1b[6~".to_vec()),
                NamedKey::Insert => Some(b"\x1b[2~".to_vec()),
                NamedKey::Delete => Some(b"\x1b[3~".to_vec()),
                _ => None,
            },
            // 文字キー（Ctrl修飾キーの処理を含む）
            Key::Character(c) => {
                // Cmd+キーは既に処理済み
                if super_key {
                    return WindowCommand::None;
                }

                // IME関連のキー（textがない）はスキップ
                if event.text.is_none() && !ctrl {
                    log::debug!("Skipping key without text: {:?}", c);
                    return WindowCommand::None;
                }

                // ASCII印刷可能文字以外はスキップ（日本語はIME::Commitで送信される）
                if !ctrl {
                    if let Some(ch) = c.chars().next() {
                        // ASCII印刷可能文字（0x20-0x7E）以外はスキップ
                        if !(ch >= ' ' && ch <= '~') {
                            log::info!("Skipping non-ASCII char: '{}' U+{:04X}", ch, ch as u32);
                            return WindowCommand::None;
                        }
                    }
                }

                if ctrl {
                    // Ctrl+文字 の処理
                    let ch = c.chars().next().unwrap_or(' ');
                    match ch.to_ascii_lowercase() {
                        'c' => Some(vec![0x03]), // Ctrl+C (ETX)
                        'd' => Some(vec![0x04]), // Ctrl+D (EOT)
                        'z' => Some(vec![0x1a]), // Ctrl+Z (SUB)
                        'l' => Some(vec![0x0c]), // Ctrl+L (FF - clear screen)
                        'a' => Some(vec![0x01]), // Ctrl+A
                        'e' => Some(vec![0x05]), // Ctrl+E
                        'u' => Some(vec![0x15]), // Ctrl+U
                        'k' => Some(vec![0x0b]), // Ctrl+K
                        'w' => Some(vec![0x17]), // Ctrl+W
                        'r' => Some(vec![0x12]), // Ctrl+R
                        _ => None,
                    }
                } else {
                    // 通常の文字入力（textフィールドを使用）
                    event.text.as_ref().map(|t| t.as_bytes().to_vec())
                }
            }
            // Dead key（IME入力開始など）は無視
            Key::Dead(_) => None,
            _ => None,
        };

        // フォーカス中のペインにキー入力を送信
        if let Some(bytes) = bytes {
            if let Some(pane) = self.panes.get(&self.focused_pane) {
                if bytes.len() == 1 && bytes[0] > 0x7f {
                    log::warn!("Sending non-ASCII byte: 0x{:02X}", bytes[0]);
                } else if bytes.iter().any(|&b| b > 0x7f) {
                    log::info!("Sending bytes: {:?} = {:?}", bytes, String::from_utf8_lossy(&bytes));
                }
                let _ = pane.pty.write(&bytes);
            }
        }

        WindowCommand::None
    }

    /// IME入力を処理（日本語入力など）
    fn handle_ime(&mut self, ime: &Ime) {
        match ime {
            Ime::Commit(text) => {
                log::info!("IME Commit: {:?}", text);
                if text.is_empty() {
                    self.ime_active = false;
                    return;
                }
                let filtered: String = text.chars()
                    .filter(|&c| c >= ' ' && c != '\u{2020}' && c != '\u{2021}')
                    .collect();
                if !filtered.is_empty() {
                    if let Some(pane) = self.panes.get(&self.focused_pane) {
                        let _ = pane.pty.write(filtered.as_bytes());
                    }
                }
                self.ime_active = false;
            }
            Ime::Preedit(text, _cursor) => {
                self.ime_active = !text.is_empty();
                self.update_ime_cursor_area();
            }
            Ime::Enabled => {
                self.ime_active = true;
                self.update_ime_cursor_area();
            }
            Ime::Disabled => {
                self.ime_active = false;
            }
        }
    }

    /// IMEカーソルエリアを更新
    fn update_ime_cursor_area(&self) {
        if let Some(pane) = self.panes.get(&self.focused_pane) {
            let terminal = pane.terminal.lock();
            let (cell_width, cell_height) = self.renderer.cell_size();

            // ペインの矩形領域を取得
            let rects = self.layout.calculate_rects(Rect::full());
            let (screen_width, screen_height) = self.renderer.screen_size();

            if let Some((_, rect)) = rects.iter().find(|(id, _)| *id == self.focused_pane) {
                let vp_x = rect.x * screen_width as f32;
                let vp_y = rect.y * screen_height as f32;

                let x = terminal.cursor.col as f32 * cell_width + vp_x;
                let y = terminal.cursor.row as f32 * cell_height + vp_y;

                let position = PhysicalPosition::new(x as u32, y as u32);
                let size = PhysicalSize::new(cell_width as u32, cell_height as u32);

                self.window.set_ime_cursor_area(position, size);
            }
        }
    }

    /// リサイズを処理
    fn handle_resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        self.renderer.resize(width, height);

        // 各ペインをリサイズ
        let rects = self.layout.calculate_rects(Rect::full());
        for (pane_id, rect) in rects {
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                let vp_width = rect.width * width as f32;
                let vp_height = rect.height * height as f32;
                let (cols, rows) = self.renderer.calculate_terminal_size_for_viewport(vp_width, vp_height);
                pane.resize(cols, rows);
            }
        }
    }

    /// マウス移動を処理
    fn handle_cursor_moved(&mut self, x: f64, y: f64) {
        let (width, height) = self.renderer.screen_size();

        // 正規化座標に変換
        let norm_x = (x as f32) / (width as f32);
        let norm_y = (y as f32) / (height as f32);
        self.mouse_pos = (norm_x, norm_y);

        // ドラッグ中なら境界線を移動
        if let Some(ref border) = self.dragging_border {
            let path = border.path().to_vec();
            let new_ratio = if border.is_vertical() {
                norm_x
            } else {
                norm_y
            };
            self.layout.update_ratio(&path, new_ratio);

            // ペインをリサイズ
            self.resize_all_panes();
            return;
        }

        // 境界線上ならカーソルを変更
        if let Some(border) = self.layout.border_at(norm_x, norm_y, Rect::full(), BORDER_THRESHOLD) {
            let cursor = if border.is_vertical() {
                CursorIcon::ColResize
            } else {
                CursorIcon::RowResize
            };
            self.window.set_cursor(cursor);
        } else {
            self.window.set_cursor(CursorIcon::Default);
        }
    }

    /// マウスボタンを処理
    fn handle_mouse_input(&mut self, button: MouseButton, state: ElementState) {
        if button != MouseButton::Left {
            return;
        }

        let (norm_x, norm_y) = self.mouse_pos;

        match state {
            ElementState::Pressed => {
                // 境界線上ならドラッグ開始
                if let Some(border) = self.layout.border_at(norm_x, norm_y, Rect::full(), BORDER_THRESHOLD) {
                    self.dragging_border = Some(border);
                    return;
                }

                // ペイン上ならフォーカス切り替え
                if let Some(pane_id) = self.layout.pane_at(norm_x, norm_y, Rect::full()) {
                    if pane_id != self.focused_pane {
                        self.focused_pane = pane_id;
                        log::info!("クリックでフォーカス切り替え: {:?}", pane_id);
                    }
                }
            }
            ElementState::Released => {
                // ドラッグ終了
                if self.dragging_border.is_some() {
                    self.dragging_border = None;
                    self.window.set_cursor(CursorIcon::Default);
                }
            }
        }
    }

    /// すべてのペインをリサイズ
    fn resize_all_panes(&mut self) {
        let (width, height) = self.renderer.screen_size();
        let rects = self.layout.calculate_rects(Rect::full());

        for (pane_id, rect) in rects {
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                let vp_width = rect.width * width as f32;
                let vp_height = rect.height * height as f32;
                let (cols, rows) = self.renderer.calculate_terminal_size_for_viewport(vp_width, vp_height);
                pane.resize(cols, rows);
            }
        }
    }
}

/// ウィンドウコマンド（キー入力の結果）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowCommand {
    None,
    NewWindow,
    ClosePane,
    SplitHorizontal,
    SplitVertical,
    FocusNextPane,
    FocusPrevPane,
}

impl App {
    /// 新しいアプリケーションを作成
    fn new() -> Self {
        // wgpu インスタンスを作成
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        Self {
            windows: HashMap::new(),
            instance,
            adapter: None,
            should_exit: false,
        }
    }

    /// 新しいウィンドウを作成
    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<WindowId> {
        // ウィンドウを作成
        let window_attrs = Window::default_attributes()
            .with_title("UmiTerm")
            .with_inner_size(winit::dpi::LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));

        let window = Arc::new(event_loop.create_window(window_attrs)?);
        let window_id = window.id();
        let size = window.inner_size();

        // サーフェスを作成
        let surface: wgpu::Surface<'static> = unsafe {
            std::mem::transmute(self.instance.create_surface(Arc::clone(&window))?)
        };

        // アダプターを取得（初回のみ）
        if self.adapter.is_none() {
            self.adapter = Some(pollster::block_on(self.instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            }))?);
        }

        let adapter = self.adapter.as_ref().context("GPUアダプターが見つかりません")?;

        // レンダラーを作成
        let renderer = pollster::block_on(Renderer::new(
            surface,
            size.width,
            size.height,
            adapter,
        ))?;

        // ターミナルサイズを計算
        let (cols, rows) = renderer.calculate_terminal_size();

        // 初期ペインを作成
        let mut initial_pane = Pane::new(cols, rows)?;
        let initial_pane_id = initial_pane.id;
        WindowState::show_startup_banner(&mut initial_pane);

        // ペインを登録
        let mut panes = std::collections::HashMap::new();
        panes.insert(initial_pane_id, initial_pane);

        // IME（日本語入力）を有効化
        window.set_ime_allowed(true);

        // WindowStateを作成
        let state = WindowState {
            window,
            renderer,
            panes,
            layout: PaneLayout::single(initial_pane_id),
            focused_pane: initial_pane_id,
            last_frame: Instant::now(),
            ime_active: false,
            modifiers: Modifiers::default(),
            mouse_pos: (0.0, 0.0),
            dragging_border: None,
        };

        // ウィンドウを登録
        self.windows.insert(window_id, state);

        log::info!("新しいウィンドウを作成しました: {:?}", window_id);

        Ok(window_id)
    }

    /// ウィンドウを閉じる
    fn close_window(&mut self, window_id: WindowId) {
        if let Some(_state) = self.windows.remove(&window_id) {
            log::info!("ウィンドウを閉じました: {:?}", window_id);
        }

        // すべてのウィンドウが閉じられたら終了
        if self.windows.is_empty() {
            log::info!("すべてのウィンドウが閉じられました。終了します。");
            self.should_exit = true;
        }
    }
}

// winit のイベントハンドラーを実装
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // 初回起動時にウィンドウを作成
        if self.windows.is_empty() {
            if let Err(e) = self.create_window(event_loop) {
                log::error!("初期化エラー: {}", e);
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // ウィンドウコマンド（新規作成・閉じるなど）を一時保存
        let mut command = WindowCommand::None;

        // 対象ウィンドウの処理
        if let Some(state) = self.windows.get_mut(&window_id) {
            match event {
                WindowEvent::CloseRequested => {
                    // ウィンドウの閉じるボタンはウィンドウ全体を閉じる
                    self.close_window(window_id);
                    return;
                }
                WindowEvent::Resized(size) => {
                    state.handle_resize(size.width, size.height);
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    command = state.handle_key(&event);
                }
                WindowEvent::ModifiersChanged(modifiers) => {
                    state.modifiers = modifiers;
                }
                WindowEvent::Ime(ime) => {
                    state.handle_ime(&ime);
                }
                WindowEvent::CursorMoved { position, .. } => {
                    state.handle_cursor_moved(position.x, position.y);
                }
                WindowEvent::MouseInput { button, state: button_state, .. } => {
                    state.handle_mouse_input(button, button_state);
                }
                WindowEvent::RedrawRequested => {
                    state.update();
                    if !state.render() {
                        self.should_exit = true;
                    }

                    // 次のフレームをリクエスト
                    state.window.request_redraw();
                }
                _ => {}
            }
        }

        // ウィンドウコマンドを処理（borrowを避けるため別途処理）
        match command {
            WindowCommand::NewWindow => {
                if let Err(e) = self.create_window(event_loop) {
                    log::error!("新規ウィンドウの作成に失敗: {}", e);
                }
            }
            WindowCommand::ClosePane => {
                // ペインを閉じる（ペインが1つならウィンドウを閉じる）
                if let Some(state) = self.windows.get_mut(&window_id) {
                    if state.close_pane() {
                        self.close_window(window_id);
                    }
                }
            }
            WindowCommand::SplitHorizontal => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    if let Err(e) = state.split_horizontal() {
                        log::error!("縦分割に失敗: {}", e);
                    }
                }
            }
            WindowCommand::SplitVertical => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    if let Err(e) = state.split_vertical() {
                        log::error!("横分割に失敗: {}", e);
                    }
                }
            }
            WindowCommand::FocusNextPane => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.focus_next_pane();
                }
            }
            WindowCommand::FocusPrevPane => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.focus_prev_pane();
                }
            }
            WindowCommand::None => {}
        }

        if self.should_exit {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // 継続的な更新をリクエスト
        for state in self.windows.values() {
            state.window.request_redraw();
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// メイン関数
// ═══════════════════════════════════════════════════════════════════════════

fn main() -> Result<()> {
    // ログを初期化
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("UmiTerm を起動中...");

    // イベントループを作成
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    // アプリケーションを作成して実行
    let mut app = App::new();
    event_loop.run_app(&mut app)?;

    log::info!("UmiTerm を終了しました");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// テスト
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::AnsiParser;

    #[test]
    fn test_terminal_creation() {
        let terminal = Terminal::new(80, 24);
        assert_eq!(terminal.active_grid().cols, 80);
        assert_eq!(terminal.active_grid().rows, 24);
    }

    #[test]
    fn test_parser_integration() {
        let mut terminal = Terminal::new(80, 24);
        let mut parser = AnsiParser::new();

        // カラフルなテキストを入力
        parser.process(&mut terminal, b"\x1b[31mRed\x1b[0m Normal");

        // 確認
        assert_eq!(terminal.active_grid()[(0, 0)].character, 'R');
    }
}
