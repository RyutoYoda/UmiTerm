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
    event::{ElementState, KeyEvent, WindowEvent},
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

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.pty = Some(pty);

        Ok(())
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
            // 文字キー
            Key::Character(c) => Some(c.as_bytes().to_vec()),
            _ => None,
        };

        if let (Some(bytes), Some(pty)) = (bytes, &self.pty) {
            let _ = pty.write(&bytes);
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
