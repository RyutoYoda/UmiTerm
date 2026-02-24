//! UmiTerm - 水色テーマの高速ターミナルエミュレータ
//!
//! # アーキテクチャ
//!
//! ```
//! ┌─────────────────────────────────────────────────────────────┐
//! │                         UmiTerm                             │
//! ├─────────────────────────────────────────────────────────────┤
//! │  ┌──────────┐    ┌──────────┐    ┌──────────────────────┐  │
//! │  │   PTY    │◄──►│ Terminal │◄──►│     GPU Renderer     │  │
//! │  │ (Shell)  │    │ (State)  │    │ (wgpu + インスタンス) │  │
//! │  └──────────┘    └──────────┘    └──────────────────────┘  │
//! │       ▲              ▲                     ▲               │
//! │       │              │                     │               │
//! │       ▼              ▼                     ▼               │
//! │  ┌──────────┐    ┌──────────┐    ┌──────────────────────┐  │
//! │  │  ANSI    │    │   Grid   │    │      Window          │  │
//! │  │  Parser  │    │ (Buffer) │    │  (winit + events)    │  │
//! │  └──────────┘    └──────────┘    └──────────────────────┘  │
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

mod grid;
mod parser;
mod pty;
mod renderer;
mod terminal;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, KeyEvent, Modifiers, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

use crate::parser::AnsiParser;
use crate::pty::Pty;
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
    "  ≋ GPU-Accelerated Terminal Emulator                     v0.1\r\n",
    "\x1b[38;2;60;180;170m",
    "  ─────────────────────────────────────────────────────────────\r\n",
    "\x1b[0m",
    "\r\n",
);

// ═══════════════════════════════════════════════════════════════════════════
// アプリケーション状態
// ═══════════════════════════════════════════════════════════════════════════

/// アプリケーション全体の状態
struct App {
    /// ウィンドウ（初期化後に設定）
    window: Option<Arc<Window>>,
    /// GPU レンダラー（サーフェスを内部で保持）
    renderer: Option<Renderer>,
    /// ターミナル状態
    terminal: Arc<Mutex<Terminal>>,
    /// ANSI パーサー
    parser: AnsiParser,
    /// PTY（擬似端末）
    pty: Option<Pty>,
    /// 最後のフレーム時刻
    last_frame: Instant,
    /// 終了フラグ
    should_exit: bool,
    /// IME入力中フラグ
    ime_active: bool,
    /// 修飾キーの状態
    modifiers: Modifiers,
}

impl App {
    /// 新しいアプリケーションを作成
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            terminal: Arc::new(Mutex::new(Terminal::new(80, 24))),
            parser: AnsiParser::new(),
            pty: None,
            last_frame: Instant::now(),
            should_exit: false,
            ime_active: false,
            modifiers: Modifiers::default(),
        }
    }

    /// ウィンドウを初期化
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        // ウィンドウを作成
        let window_attrs = Window::default_attributes()
            .with_title("UmiTerm")
            .with_inner_size(winit::dpi::LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));

        let window = Arc::new(event_loop.create_window(window_attrs)?);
        let size = window.inner_size();

        // wgpu インスタンスを作成
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        // サーフェスを作成（1つだけ作成してRendererに渡す）
        let surface: wgpu::Surface<'static> = unsafe {
            std::mem::transmute(instance.create_surface(Arc::clone(&window))?)
        };

        // アダプターを取得
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .context("GPUアダプターが見つかりません")?;

        // レンダラーを作成（サーフェスを所有させる）
        let renderer = pollster::block_on(Renderer::new(
            surface,
            size.width,
            size.height,
            &adapter,
        ))?;

        // ターミナルサイズを計算
        let (cols, rows) = renderer.calculate_terminal_size();

        // ターミナルをリサイズ
        {
            let mut terminal = self.terminal.lock();
            terminal.resize(cols as usize, rows as usize);
        }

        // PTYを起動
        let pty = Pty::spawn(cols, rows, None)?;

        // IME（日本語入力）を有効化
        window.set_ime_allowed(true);

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.pty = Some(pty);

        // 起動バナーを表示
        self.show_startup_banner();

        Ok(())
    }

    /// 起動バナーを表示し、カーソルを下部に移動
    fn show_startup_banner(&mut self) {
        let mut terminal = self.terminal.lock();
        self.parser.process(&mut terminal, STARTUP_BANNER.as_bytes());

        // カーソルを画面下部に移動（空行を挿入）
        let rows = terminal.active_grid().rows;
        let current_row = terminal.cursor.row;
        let lines_to_add = rows.saturating_sub(current_row + 3);
        for _ in 0..lines_to_add {
            self.parser.process(&mut terminal, b"\r\n");
        }
    }

    /// フレームを更新
    fn update(&mut self) {
        // PTYからの出力を読み取り
        if let Some(ref pty) = self.pty {
            if let Some(data) = pty.read() {
                let mut terminal = self.terminal.lock();
                self.parser.process(&mut terminal, &data);
            }
        }
    }

    /// 描画
    fn render(&mut self) {
        // フレームレート制限
        let now = Instant::now();
        if now - self.last_frame < MIN_FRAME_INTERVAL {
            return;
        }
        self.last_frame = now;

        if let Some(renderer) = &mut self.renderer {
            let terminal = self.terminal.lock();
            match renderer.render(&terminal) {
                Ok(_) => {}
                Err(wgpu::SurfaceError::Lost) => {
                    // サーフェスを再設定
                    if let Some(window) = &self.window {
                        let size = window.inner_size();
                        renderer.resize(size.width, size.height);
                    }
                }
                Err(wgpu::SurfaceError::OutOfMemory) => {
                    log::error!("GPUメモリ不足");
                    self.should_exit = true;
                }
                Err(e) => {
                    log::warn!("描画エラー: {:?}", e);
                }
            }
        }
    }

    /// キー入力を処理
    fn handle_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }

        // IME入力中はキーイベントをスキップ（ただし特殊キーは通す）
        if self.ime_active {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) |
                Key::Named(NamedKey::Enter) |
                Key::Named(NamedKey::Backspace) => {
                    // これらは通す
                }
                _ => return,
            }
        }

        let ctrl = self.modifiers.state().control_key();

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
                // IME関連のキー（textがない）はスキップ
                if event.text.is_none() && !ctrl {
                    log::debug!("Skipping key without text: {:?}", c);
                    return;
                }

                // ASCII印刷可能文字以外はスキップ（日本語はIME::Commitで送信される）
                if !ctrl {
                    if let Some(ch) = c.chars().next() {
                        // ASCII印刷可能文字（0x20-0x7E）以外はスキップ
                        if !(ch >= ' ' && ch <= '~') {
                            log::info!("Skipping non-ASCII char: '{}' U+{:04X}", ch, ch as u32);
                            return;
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

        if let (Some(bytes), Some(pty)) = (bytes, &self.pty) {
            // デバッグ: 送信するバイトをログ出力
            if bytes.len() == 1 && bytes[0] > 0x7f {
                log::warn!("Sending non-ASCII byte: 0x{:02X}", bytes[0]);
            } else if bytes.iter().any(|&b| b > 0x7f) {
                log::info!("Sending bytes: {:?} = {:?}", bytes, String::from_utf8_lossy(&bytes));
            }
            let _ = pty.write(&bytes);
        }
    }

    /// IME入力を処理（日本語入力など）
    fn handle_ime(&mut self, ime: &Ime) {
        match ime {
            Ime::Commit(text) => {
                log::info!("IME Commit: {:?}", text);
                // 空文字や制御文字のみの場合はスキップ
                if text.is_empty() {
                    self.ime_active = false;
                    return;
                }
                // †（U+2020）などの特殊文字をフィルタリング
                let filtered: String = text.chars()
                    .filter(|&c| c >= ' ' && c != '\u{2020}' && c != '\u{2021}')
                    .collect();
                if !filtered.is_empty() {
                    if let Some(pty) = &self.pty {
                        let _ = pty.write(filtered.as_bytes());
                    }
                }
                self.ime_active = false;
            }
            Ime::Preedit(text, _cursor) => {
                // 未確定テキスト（変換中）
                self.ime_active = !text.is_empty();
                // IMEカーソル位置を更新
                self.update_ime_cursor_area();
            }
            Ime::Enabled => {
                // IME有効時もアクティブに設定
                self.ime_active = true;
                // IMEカーソル位置を設定
                self.update_ime_cursor_area();
            }
            Ime::Disabled => {
                self.ime_active = false;
            }
        }
    }

    /// IMEカーソルエリアを更新（変換候補ウィンドウの位置）
    fn update_ime_cursor_area(&self) {
        if let (Some(window), Some(renderer)) = (&self.window, &self.renderer) {
            let terminal = self.terminal.lock();
            let (cell_width, cell_height) = renderer.cell_size();

            // カーソル位置をピクセル座標に変換
            let x = terminal.cursor.col as f32 * cell_width;
            let y = terminal.cursor.row as f32 * cell_height;

            let position = PhysicalPosition::new(x as u32, y as u32);
            let size = PhysicalSize::new(cell_width as u32, cell_height as u32);

            window.set_ime_cursor_area(position, size);
        }
    }

    /// リサイズを処理
    fn handle_resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        if let Some(renderer) = &mut self.renderer {
            renderer.resize(width, height);

            let (cols, rows) = renderer.calculate_terminal_size();

            // ターミナルをリサイズ
            {
                let mut terminal = self.terminal.lock();
                terminal.resize(cols as usize, rows as usize);
            }

            // PTYをリサイズ
            if let Some(pty) = &mut self.pty {
                let _ = pty.resize(cols, rows);
            }
        }
    }
}

// winit のイベントハンドラーを実装
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            if let Err(e) = self.initialize(event_loop) {
                log::error!("初期化エラー: {}", e);
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.handle_resize(size.width, size.height);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_key(&event);
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
            }
            WindowEvent::Ime(ime) => {
                self.handle_ime(&ime);
            }
            WindowEvent::RedrawRequested => {
                self.update();
                self.render();

                // 次のフレームをリクエスト
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }

        if self.should_exit {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // 継続的な更新をリクエスト
        if let Some(window) = &self.window {
            window.request_redraw();
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
