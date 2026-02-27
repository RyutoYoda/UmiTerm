//! ANSI エスケープシーケンスパーサー
//!
//! vte クレートを使用して高速にパース
//! CSI, OSC, DCS などのシーケンスを処理

use std::path::PathBuf;
use vte::{Params, Parser, Perform};

use crate::grid::{CellFlags, Color};
use crate::terminal::{CursorShape, Terminal, TerminalMode};

// ═══════════════════════════════════════════════════════════════════════════
// パーサー構造体
// ═══════════════════════════════════════════════════════════════════════════

/// ANSIパーサー
/// vteパーサーとターミナルをつなぐアダプター
pub struct AnsiParser {
    /// vte パーサー（状態マシン）
    parser: Parser,
}

impl AnsiParser {
    /// 新しいパーサーを作成
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
        }
    }

    /// バイト列をパースしてターミナルに適用
    pub fn process(&mut self, terminal: &mut Terminal, data: &[u8]) {
        let mut performer = TerminalPerformer { terminal };
        for byte in data {
            self.parser.advance(&mut performer, *byte);
        }
    }
}

impl Default for AnsiParser {
    fn default() -> Self {
        Self::new()
    }
}

/// OSC 7のfile:// URLからパスを抽出
/// 形式: file://hostname/path または file:///path
fn parse_osc7_path(url: &str) -> Option<PathBuf> {
    // file:// で始まるかチェック
    let rest = url.strip_prefix("file://")?;

    // ホスト名をスキップ（最初の/まで）
    let path_start = if rest.starts_with('/') {
        // file:///path の形式（ホスト名なし）
        0
    } else {
        // file://hostname/path の形式
        rest.find('/')?
    };

    let path_str = &rest[path_start..];

    // URLデコード（%20 -> スペース など）
    let decoded = url_decode(path_str);

    Some(PathBuf::from(decoded))
}

/// 簡易的なURLデコード
fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            // 次の2文字を16進数として読む
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            result.push('%');
            result.push_str(&hex);
        } else {
            result.push(c);
        }
    }

    result
}

// ═══════════════════════════════════════════════════════════════════════════
// パフォーマー（vteのコールバックを実装）
// ═══════════════════════════════════════════════════════════════════════════

/// vte の Perform トレイトを実装
/// パーサーからのコールバックを受け取り、ターミナルを操作
struct TerminalPerformer<'a> {
    terminal: &'a mut Terminal,
}

impl<'a> Perform for TerminalPerformer<'a> {
    /// 通常の文字を処理
    fn print(&mut self, c: char) {
        // †（U+2020）などの特殊文字をスキップ（Claude Code等が送信する場合がある）
        if c == '\u{2020}' || c == '\u{2021}' {
            return;
        }
        self.terminal.input_char(c);
    }

    /// 制御文字を処理（C0/C1）
    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {} // BEL (ベル) - 無視
            0x08 => self.terminal.backspace(),
            0x09 => self.terminal.tab(),
            0x0A | 0x0B | 0x0C => self.terminal.linefeed(),
            0x0D => self.terminal.carriage_return(),
            _ => {}
        }
    }

    /// CSI シーケンスを処理（\x1B[...）
    fn csi_dispatch(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        // DEC private mode（?がある場合）
        let is_private = intermediates.contains(&b'?');
        // パラメータを Vec に変換（複数のパラメータに対応）
        let params: Vec<u16> = params
            .iter()
            .map(|p| p.first().copied().unwrap_or(0))
            .collect();

        // デフォルト値を取得するヘルパー
        let get = |idx: usize, default: u16| -> usize {
            params.get(idx).copied().unwrap_or(default) as usize
        };

        match action {
            // ─────────────────────────────────────────────────────────────────
            // カーソル移動
            // ─────────────────────────────────────────────────────────────────
            'A' => {
                // CUU: カーソルを上に移動
                let n = get(0, 1);
                self.terminal.move_cursor(0, -(n as i32));
            }
            'B' => {
                // CUD: カーソルを下に移動
                let n = get(0, 1);
                self.terminal.move_cursor(0, n as i32);
            }
            'C' => {
                // CUF: カーソルを右に移動
                let n = get(0, 1);
                self.terminal.move_cursor(n as i32, 0);
            }
            'D' => {
                // CUB: カーソルを左に移動
                let n = get(0, 1);
                self.terminal.move_cursor(-(n as i32), 0);
            }
            'E' => {
                // CNL: カーソルを下のn行の先頭に移動
                let n = get(0, 1);
                self.terminal.move_cursor(0, n as i32);
                self.terminal.carriage_return();
            }
            'F' => {
                // CPL: カーソルを上のn行の先頭に移動
                let n = get(0, 1);
                self.terminal.move_cursor(0, -(n as i32));
                self.terminal.carriage_return();
            }
            'G' => {
                // CHA: カーソルを指定列に移動
                let col = get(0, 1).saturating_sub(1);
                self.terminal.cursor.col = col;
            }
            'H' | 'f' => {
                // CUP: カーソルを指定位置に移動
                let row = get(0, 1).saturating_sub(1);
                let col = get(1, 1).saturating_sub(1);
                self.terminal.move_cursor_to(col, row);
            }

            // ─────────────────────────────────────────────────────────────────
            // 消去
            // ─────────────────────────────────────────────────────────────────
            'J' => {
                // ED: 画面消去
                match get(0, 0) {
                    0 => self.terminal.erase_display_to_end(),
                    1 => self.terminal.erase_display_to_start(),
                    2 | 3 => self.terminal.erase_display(),
                    _ => {}
                }
            }
            'K' => {
                // EL: 行消去
                match get(0, 0) {
                    0 => self.terminal.erase_line_to_end(),
                    1 => self.terminal.erase_line_to_start(),
                    2 => self.terminal.erase_line(),
                    _ => {}
                }
            }

            // ─────────────────────────────────────────────────────────────────
            // スクロール
            // ─────────────────────────────────────────────────────────────────
            'S' => {
                // SU: スクロールアップ
                let n = get(0, 1);
                self.terminal.scroll_up(n);
            }
            'T' => {
                // SD: スクロールダウン
                let n = get(0, 1);
                self.terminal.scroll_down(n);
            }

            // ─────────────────────────────────────────────────────────────────
            // SGR（文字属性）
            // ─────────────────────────────────────────────────────────────────
            'm' => self.handle_sgr(&params),

            // ─────────────────────────────────────────────────────────────────
            // スクロール領域
            // ─────────────────────────────────────────────────────────────────
            'r' => {
                // DECSTBM: スクロール領域を設定
                let rows = self.terminal.active_grid().rows;
                let top = get(0, 1).saturating_sub(1);
                let bottom = get(1, rows as u16) as usize;
                self.terminal.scroll_top = top;
                self.terminal.scroll_bottom = bottom.saturating_sub(1).min(rows - 1);
                self.terminal.move_cursor_to(0, 0);
            }

            // ─────────────────────────────────────────────────────────────────
            // カーソル保存/復元
            // ─────────────────────────────────────────────────────────────────
            's' => self.terminal.save_cursor(),
            'u' => self.terminal.restore_cursor(),

            // ─────────────────────────────────────────────────────────────────
            // モード設定（DECSET/DECRST）
            // ─────────────────────────────────────────────────────────────────
            'h' => self.handle_mode(true, &params, is_private),
            'l' => self.handle_mode(false, &params, is_private),

            // ─────────────────────────────────────────────────────────────────
            // カーソル形状
            // ─────────────────────────────────────────────────────────────────
            'q' => {
                // DECSCUSR: カーソル形状を設定
                let shape = match get(0, 0) {
                    0 | 1 => CursorShape::Block,
                    2 => CursorShape::Block,
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Beam,
                    _ => CursorShape::Block,
                };
                self.terminal.cursor.shape = shape;
            }

            // ─────────────────────────────────────────────────────────────────
            // デバイスステータス報告（DSR）
            // ─────────────────────────────────────────────────────────────────
            'n' => {
                match get(0, 0) {
                    5 => {
                        // DSR: ターミナル状態報告 → "OK"を返す
                        self.terminal.queue_response(b"\x1b[0n");
                    }
                    6 => {
                        // DSR: カーソル位置報告
                        self.terminal.report_cursor_position();
                    }
                    _ => {}
                }
            }

            _ => {
                log::debug!("未対応のCSI: {}", action);
            }
        }
    }

    /// OSC シーケンスを処理（\x1B]...）
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }

        // 最初のパラメータはOSCコード
        let code = params[0];
        let code_str = std::str::from_utf8(code).unwrap_or("");
        let code_num: u32 = code_str.parse().unwrap_or(0);

        match code_num {
            // ウィンドウタイトル
            0 | 2 => {
                if params.len() > 1 {
                    if let Ok(title) = std::str::from_utf8(params[1]) {
                        self.terminal.title = title.to_string();
                    }
                }
            }
            // 現在の作業ディレクトリ（OSC 7）
            // 形式: file://hostname/path または file:///path
            7 => {
                if params.len() > 1 {
                    if let Ok(url) = std::str::from_utf8(params[1]) {
                        if let Some(path) = parse_osc7_path(url) {
                            self.terminal.cwd = path;
                        }
                    }
                }
            }
            // その他のOSCは無視
            _ => {}
        }
    }

    /// フックの開始（DCS等）
    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}

    /// フックデータ
    fn put(&mut self, _byte: u8) {}

    /// フックの終了
    fn unhook(&mut self) {}

    /// ESC シーケンス
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.terminal.save_cursor(),    // DECSC
            b'8' => self.terminal.restore_cursor(), // DECRC
            b'D' => self.terminal.linefeed(),       // IND
            b'E' => {                               // NEL
                self.terminal.linefeed();
                self.terminal.carriage_return();
            }
            b'M' => self.terminal.scroll_down(1),   // RI
            b'c' => {                               // RIS (フルリセット)
                let (cols, rows) = (
                    self.terminal.active_grid().cols,
                    self.terminal.active_grid().rows,
                );
                *self.terminal = Terminal::new(cols, rows);
            }
            _ => {}
        }
    }
}

impl<'a> TerminalPerformer<'a> {
    /// SGR（Select Graphic Rendition）を処理
    fn handle_sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            // パラメータなしはリセット
            self.terminal.current_style.fg = Color::EMERALD;
            self.terminal.current_style.bg = Color::BLACK;
            self.terminal.current_style.flags = CellFlags::empty();
            return;
        }

        let mut i = 0;
        while i < params.len() {
            match params[i] {
                // リセット
                0 => {
                    self.terminal.current_style.fg = Color::EMERALD;
                    self.terminal.current_style.bg = Color::BLACK;
                    self.terminal.current_style.flags = CellFlags::empty();
                }
                // スタイル設定
                1 => self.terminal.current_style.flags.insert(CellFlags::BOLD),
                3 => self.terminal.current_style.flags.insert(CellFlags::ITALIC),
                4 => self.terminal.current_style.flags.insert(CellFlags::UNDERLINE),
                5 => self.terminal.current_style.flags.insert(CellFlags::BLINK),
                7 => self.terminal.current_style.flags.insert(CellFlags::INVERSE),
                8 => self.terminal.current_style.flags.insert(CellFlags::HIDDEN),
                9 => self.terminal.current_style.flags.insert(CellFlags::STRIKEOUT),
                // スタイル解除
                22 => self.terminal.current_style.flags.remove(CellFlags::BOLD),
                23 => self.terminal.current_style.flags.remove(CellFlags::ITALIC),
                24 => self.terminal.current_style.flags.remove(CellFlags::UNDERLINE),
                25 => self.terminal.current_style.flags.remove(CellFlags::BLINK),
                27 => self.terminal.current_style.flags.remove(CellFlags::INVERSE),
                28 => self.terminal.current_style.flags.remove(CellFlags::HIDDEN),
                29 => self.terminal.current_style.flags.remove(CellFlags::STRIKEOUT),
                // 前景色（標準8色）
                30 => self.terminal.current_style.fg = Color::BLACK,
                31 => self.terminal.current_style.fg = Color::RED,
                32 => self.terminal.current_style.fg = Color::GREEN,
                33 => self.terminal.current_style.fg = Color::YELLOW,
                34 => self.terminal.current_style.fg = Color::BLUE,
                35 => self.terminal.current_style.fg = Color::MAGENTA,
                36 => self.terminal.current_style.fg = Color::CYAN,
                37 => self.terminal.current_style.fg = Color::WHITE,
                // 拡張前景色
                38 => {
                    if let Some(color) = self.parse_extended_color(&params[i..]) {
                        self.terminal.current_style.fg = color;
                        i += self.extended_color_params(&params[i..]);
                    }
                }
                39 => self.terminal.current_style.fg = Color::EMERALD, // デフォルト前景色
                // 背景色（標準8色）
                40 => self.terminal.current_style.bg = Color::BLACK,
                41 => self.terminal.current_style.bg = Color::RED,
                42 => self.terminal.current_style.bg = Color::GREEN,
                43 => self.terminal.current_style.bg = Color::YELLOW,
                44 => self.terminal.current_style.bg = Color::BLUE,
                45 => self.terminal.current_style.bg = Color::MAGENTA,
                46 => self.terminal.current_style.bg = Color::CYAN,
                47 => self.terminal.current_style.bg = Color::WHITE,
                // 拡張背景色
                48 => {
                    if let Some(color) = self.parse_extended_color(&params[i..]) {
                        self.terminal.current_style.bg = color;
                        i += self.extended_color_params(&params[i..]);
                    }
                }
                49 => self.terminal.current_style.bg = Color::BLACK, // デフォルト背景色
                // 明るい前景色
                90..=97 => {
                    let bright_colors = [
                        Color::rgb(128, 128, 128), // 明るい黒
                        Color::rgb(255, 0, 0),     // 明るい赤
                        Color::rgb(0, 255, 0),     // 明るい緑
                        Color::rgb(255, 255, 0),   // 明るい黄
                        Color::rgb(0, 0, 255),     // 明るい青
                        Color::rgb(255, 0, 255),   // 明るいマゼンタ
                        Color::rgb(0, 255, 255),   // 明るいシアン
                        Color::rgb(255, 255, 255), // 明るい白
                    ];
                    self.terminal.current_style.fg = bright_colors[(params[i] - 90) as usize];
                }
                // 明るい背景色
                100..=107 => {
                    let bright_colors = [
                        Color::rgb(128, 128, 128),
                        Color::rgb(255, 0, 0),
                        Color::rgb(0, 255, 0),
                        Color::rgb(255, 255, 0),
                        Color::rgb(0, 0, 255),
                        Color::rgb(255, 0, 255),
                        Color::rgb(0, 255, 255),
                        Color::rgb(255, 255, 255),
                    ];
                    self.terminal.current_style.bg = bright_colors[(params[i] - 100) as usize];
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// 拡張色（256色/TrueColor）をパース
    fn parse_extended_color(&self, params: &[u16]) -> Option<Color> {
        if params.len() < 2 {
            return None;
        }

        match params[1] {
            // 256色モード
            5 => {
                if params.len() >= 3 {
                    Some(Color::from_ansi256(params[2] as u8))
                } else {
                    None
                }
            }
            // TrueColor (RGB)
            2 => {
                if params.len() >= 5 {
                    Some(Color::rgb(params[2] as u8, params[3] as u8, params[4] as u8))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 拡張色パラメータの数を返す
    fn extended_color_params(&self, params: &[u16]) -> usize {
        if params.len() < 2 {
            return 0;
        }
        match params[1] {
            5 => 2,
            2 => 4,
            _ => 0,
        }
    }

    /// モード設定/解除を処理
    fn handle_mode(&mut self, enable: bool, params: &[u16], is_private: bool) {
        for &param in params {
            if is_private {
                // DEC Private Mode（CSI ? Pm h/l）
                match param {
                    // カーソルキーモード（アプリケーション）
                    1 => {
                        if enable {
                            self.terminal.mode.insert(TerminalMode::CURSOR_KEYS_APP);
                        } else {
                            self.terminal.mode.remove(TerminalMode::CURSOR_KEYS_APP);
                        }
                    }
                    // カーソル表示
                    25 => {
                        self.terminal.cursor.visible = enable;
                    }
                    // 自動改行
                    7 => {
                        if enable {
                            self.terminal.mode.insert(TerminalMode::AUTO_WRAP);
                        } else {
                            self.terminal.mode.remove(TerminalMode::AUTO_WRAP);
                        }
                    }
                    // 代替スクリーン
                    1049 | 47 | 1047 => {
                        if enable {
                            self.terminal.enter_alt_screen();
                        } else {
                            self.terminal.exit_alt_screen();
                        }
                    }
                    // ブラケットペースト
                    2004 => {
                        if enable {
                            self.terminal.mode.insert(TerminalMode::BRACKETED_PASTE);
                        } else {
                            self.terminal.mode.remove(TerminalMode::BRACKETED_PASTE);
                        }
                    }
                    // マウストラッキング
                    1000 | 1002 | 1003 | 1006 | 1015 => {
                        if enable {
                            self.terminal.mode.insert(TerminalMode::MOUSE_TRACKING);
                        } else {
                            self.terminal.mode.remove(TerminalMode::MOUSE_TRACKING);
                        }
                    }
                    _ => {
                        log::debug!("未対応のDEC private mode: {}", param);
                    }
                }
            } else {
                // Standard Mode（CSI Pm h/l）
                match param {
                    4 => {
                        // 挿入モード
                        if enable {
                            self.terminal.mode.insert(TerminalMode::INSERT);
                        } else {
                            self.terminal.mode.remove(TerminalMode::INSERT);
                        }
                    }
                    _ => {
                        log::debug!("未対応のstandard mode: {}", param);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_movement() {
        let mut terminal = Terminal::new(80, 24);
        let mut parser = AnsiParser::new();

        // カーソルを(5, 10)に移動
        parser.process(&mut terminal, b"\x1b[11;6H");

        assert_eq!(terminal.cursor.col, 5);
        assert_eq!(terminal.cursor.row, 10);
    }

    #[test]
    fn test_sgr_colors() {
        let mut terminal = Terminal::new(80, 24);
        let mut parser = AnsiParser::new();

        // 赤い前景色を設定
        parser.process(&mut terminal, b"\x1b[31m");

        assert_eq!(terminal.current_style.fg, Color::RED);
    }

    #[test]
    fn test_clear_screen() {
        let mut terminal = Terminal::new(80, 24);
        let mut parser = AnsiParser::new();

        // 文字を書いて消去
        parser.process(&mut terminal, b"Hello");
        parser.process(&mut terminal, b"\x1b[2J");

        assert_eq!(terminal.grid[(0, 0)].character, ' ');
    }
}
