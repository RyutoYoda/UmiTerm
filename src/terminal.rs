//! ターミナル状態管理モジュール
//!
//! カーソル位置、スクロール領域、モードなどの状態を管理

use std::path::PathBuf;
use unicode_width::UnicodeWidthChar;

use crate::grid::{Cell, CellFlags, Color, Grid};

// ═══════════════════════════════════════════════════════════════════════════
// カーソル
// ═══════════════════════════════════════════════════════════════════════════

/// カーソルの状態
#[derive(Clone, Debug)]
pub struct Cursor {
    /// 列位置（0始まり）
    pub col: usize,
    /// 行位置（0始まり）
    pub row: usize,
    /// カーソルの形状
    pub shape: CursorShape,
    /// 点滅するかどうか
    pub blinking: bool,
    /// 表示するかどうか
    pub visible: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            blinking: true,
            visible: true,
        }
    }
}

/// カーソルの形状
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum CursorShape {
    #[default]
    Block,      // █
    Underline,  // _
    Beam,       // |
}

// ═══════════════════════════════════════════════════════════════════════════
// テキスト選択
// ═══════════════════════════════════════════════════════════════════════════

/// テキスト選択の状態
#[derive(Clone, Debug, Default)]
pub struct Selection {
    /// 選択開始位置（アンカー）
    pub start: Option<(usize, usize)>, // (col, row)
    /// 選択終了位置
    pub end: Option<(usize, usize)>,   // (col, row)
    /// 選択中かどうか
    pub active: bool,
}

impl Selection {
    /// 選択をクリア
    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.active = false;
    }

    /// 選択を開始
    pub fn start_at(&mut self, col: usize, row: usize) {
        self.start = Some((col, row));
        self.end = Some((col, row));
        self.active = true;
    }

    /// 選択を拡張
    pub fn extend_to(&mut self, col: usize, row: usize) {
        if self.active {
            self.end = Some((col, row));
        }
    }

    /// 選択を終了
    pub fn finish(&mut self) {
        self.active = false;
    }

    /// 指定位置が選択範囲内かどうか
    pub fn contains(&self, col: usize, row: usize) -> bool {
        let (start, end) = match (self.start, self.end) {
            (Some(s), Some(e)) => {
                // 開始と終了を正規化（行順、同じ行なら列順）
                if s.1 < e.1 || (s.1 == e.1 && s.0 <= e.0) {
                    (s, e)
                } else {
                    (e, s)
                }
            }
            _ => return false,
        };

        // 行が範囲内かチェック
        if row < start.1 || row > end.1 {
            return false;
        }

        // 単一行選択
        if start.1 == end.1 {
            return col >= start.0 && col <= end.0;
        }

        // 複数行選択
        if row == start.1 {
            // 開始行: 開始列以降
            col >= start.0
        } else if row == end.1 {
            // 終了行: 終了列以前
            col <= end.0
        } else {
            // 中間行: 全列
            true
        }
    }

    /// 選択範囲があるかどうか
    pub fn has_selection(&self) -> bool {
        self.start.is_some() && self.end.is_some()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ターミナルモード
// ═══════════════════════════════════════════════════════════════════════════

bitflags::bitflags! {
    /// ターミナルのモードフラグ
    #[derive(Clone, Copy, Debug, Default)]
    pub struct TerminalMode: u32 {
        /// カーソルキーモード（アプリケーションモード）
        const CURSOR_KEYS_APP   = 0b0000_0001;
        /// 代替スクリーンバッファ
        const ALT_SCREEN        = 0b0000_0010;
        /// 自動改行
        const AUTO_WRAP         = 0b0000_0100;
        /// 挿入モード
        const INSERT            = 0b0000_1000;
        /// 原点モード
        const ORIGIN            = 0b0001_0000;
        /// マウストラッキング
        const MOUSE_TRACKING    = 0b0010_0000;
        /// ブラケットペースト
        const BRACKETED_PASTE   = 0b0100_0000;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ターミナル
// ═══════════════════════════════════════════════════════════════════════════

/// ターミナルの完全な状態
pub struct Terminal {
    /// メイングリッド
    pub grid: Grid,
    /// 代替グリッド（vim等で使用）
    pub alt_grid: Grid,
    /// カーソル
    pub cursor: Cursor,
    /// 保存されたカーソル（CSI s/u用）
    saved_cursor: Cursor,
    /// ターミナルモード
    pub mode: TerminalMode,
    /// 現在のセルスタイル（SGRで設定）
    pub current_style: CellStyle,
    /// スクロール領域の上端
    pub scroll_top: usize,
    /// スクロール領域の下端
    pub scroll_bottom: usize,
    /// タブストップ
    pub tabs: Vec<usize>,
    /// ターミナルタイトル
    pub title: String,
    /// 現在の作業ディレクトリ（OSC 7から取得）
    pub cwd: PathBuf,
    /// テキスト選択状態
    pub selection: Selection,
}

/// 現在のセルスタイル（新しい文字に適用される）
#[derive(Clone, Debug, Default)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
}

impl Terminal {
    /// 新しいターミナルを作成
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut tabs = Vec::new();
        // 8文字ごとにタブストップを設定
        for i in (8..cols).step_by(8) {
            tabs.push(i);
        }

        Self {
            grid: Grid::new(cols, rows),
            alt_grid: Grid::new(cols, rows),
            cursor: Cursor::default(),
            saved_cursor: Cursor::default(),
            mode: TerminalMode::AUTO_WRAP,
            current_style: CellStyle {
                fg: Color::EMERALD,
                bg: Color::BLACK,
                flags: CellFlags::empty(),
            },
            scroll_top: 0,
            scroll_bottom: rows - 1,
            tabs,
            title: String::from("BlazeTerm"),
            cwd: std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
            selection: Selection::default(),
        }
    }

    // ───────────────────────────────────────────────────────────────────────
    // 基本操作
    // ───────────────────────────────────────────────────────────────────────

    /// 現在のグリッドを取得
    #[inline]
    pub fn active_grid(&self) -> &Grid {
        if self.mode.contains(TerminalMode::ALT_SCREEN) {
            &self.alt_grid
        } else {
            &self.grid
        }
    }

    /// 現在のグリッドを可変参照で取得
    #[inline]
    pub fn active_grid_mut(&mut self) -> &mut Grid {
        if self.mode.contains(TerminalMode::ALT_SCREEN) {
            &mut self.alt_grid
        } else {
            &mut self.grid
        }
    }

    /// 文字を入力
    pub fn input_char(&mut self, c: char) {
        // 制御文字は別処理
        if c < ' ' {
            self.handle_control_char(c);
            return;
        }

        // 文字幅を取得（全角は2、半角は1）
        let char_width = c.width().unwrap_or(1);

        // 画面外なら無視
        let cols = self.active_grid().cols;

        // 全角文字が入りきらない場合も改行
        if self.cursor.col + char_width > cols {
            if self.mode.contains(TerminalMode::AUTO_WRAP) {
                // 自動改行
                self.cursor.col = 0;
                self.cursor.row += 1;
                if self.cursor.row > self.scroll_bottom {
                    self.scroll_up(1);
                    self.cursor.row = self.scroll_bottom;
                }
            } else {
                self.cursor.col = cols - char_width;
            }
        }

        // セルを設定
        let cell = Cell {
            character: c,
            fg: self.current_style.fg,
            bg: self.current_style.bg,
            flags: self.current_style.flags,
        };

        let col = self.cursor.col;
        let row = self.cursor.row;
        self.active_grid_mut().set(col, row, cell);

        // 全角文字の場合、2セル目を空白で埋める
        if char_width == 2 && col + 1 < cols {
            let spacer = Cell {
                character: ' ',
                fg: self.current_style.fg,
                bg: self.current_style.bg,
                flags: self.current_style.flags,
            };
            self.active_grid_mut().set(col + 1, row, spacer);
        }

        self.cursor.col += char_width;
    }

    /// 制御文字を処理
    fn handle_control_char(&mut self, c: char) {
        match c {
            '\n' => self.linefeed(),
            '\r' => self.carriage_return(),
            '\t' => self.tab(),
            '\x08' => self.backspace(), // BS
            '\x07' => {} // Bell - 無視
            _ => {}
        }
    }

    // ───────────────────────────────────────────────────────────────────────
    // カーソル移動
    // ───────────────────────────────────────────────────────────────────────

    /// カーソルを絶対位置に移動
    pub fn move_cursor_to(&mut self, col: usize, row: usize) {
        let cols = self.active_grid().cols;
        let rows = self.active_grid().rows;
        self.cursor.col = col.min(cols.saturating_sub(1));
        self.cursor.row = row.min(rows.saturating_sub(1));
    }

    /// カーソルを相対的に移動
    pub fn move_cursor(&mut self, delta_col: i32, delta_row: i32) {
        let new_col = (self.cursor.col as i32 + delta_col).max(0) as usize;
        let new_row = (self.cursor.row as i32 + delta_row).max(0) as usize;
        self.move_cursor_to(new_col, new_row);
    }

    /// カーソルを保存
    pub fn save_cursor(&mut self) {
        self.saved_cursor = self.cursor.clone();
    }

    /// カーソルを復元
    pub fn restore_cursor(&mut self) {
        self.cursor = self.saved_cursor.clone();
    }

    // ───────────────────────────────────────────────────────────────────────
    // 特殊操作
    // ───────────────────────────────────────────────────────────────────────

    /// 改行
    pub fn linefeed(&mut self) {
        if self.cursor.row >= self.scroll_bottom {
            // スクロール領域の最下行にいる場合はスクロール
            self.scroll_up(1);
        } else {
            self.cursor.row += 1;
        }
    }

    /// キャリッジリターン
    pub fn carriage_return(&mut self) {
        self.cursor.col = 0;
    }

    /// タブ
    pub fn tab(&mut self) {
        let cols = self.active_grid().cols;
        // 次のタブストップを探す
        for &stop in &self.tabs {
            if stop > self.cursor.col {
                self.cursor.col = stop.min(cols - 1);
                return;
            }
        }
        // タブストップがなければ行末へ
        self.cursor.col = cols - 1;
    }

    /// バックスペース
    pub fn backspace(&mut self) {
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
        }
    }

    /// スクロール領域をスクロールアップ
    pub fn scroll_up(&mut self, amount: usize) {
        // 借用問題を避けるためローカル変数にコピー
        let scroll_top = self.scroll_top;
        let scroll_bottom = self.scroll_bottom;
        let cols = self.active_grid().cols;

        // スクロール領域内の行を上にシフト
        for row in scroll_top..=scroll_bottom.saturating_sub(amount) {
            for col in 0..cols {
                let src_row = row + amount;
                if src_row <= scroll_bottom {
                    let cell = self.active_grid()[(col, src_row)];
                    self.active_grid_mut().set(col, row, cell);
                }
            }
        }

        // 新しい行をクリア
        for row in (scroll_bottom + 1 - amount)..=scroll_bottom {
            self.active_grid_mut().clear_row(row);
        }
    }

    /// スクロール領域をスクロールダウン
    pub fn scroll_down(&mut self, amount: usize) {
        // 借用問題を避けるためローカル変数にコピー
        let scroll_top = self.scroll_top;
        let scroll_bottom = self.scroll_bottom;
        let cols = self.active_grid().cols;

        // スクロール領域内の行を下にシフト
        for row in (scroll_top + amount..=scroll_bottom).rev() {
            for col in 0..cols {
                let src_row = row - amount;
                if src_row >= scroll_top {
                    let cell = self.active_grid()[(col, src_row)];
                    self.active_grid_mut().set(col, row, cell);
                }
            }
        }

        // 新しい行をクリア
        for row in scroll_top..scroll_top + amount {
            self.active_grid_mut().clear_row(row);
        }
    }

    // ───────────────────────────────────────────────────────────────────────
    // 消去操作
    // ───────────────────────────────────────────────────────────────────────

    /// カーソル位置から行末まで消去
    pub fn erase_line_to_end(&mut self) {
        let row = self.cursor.row;
        let cols = self.active_grid().cols;
        for col in self.cursor.col..cols {
            self.active_grid_mut().set(col, row, Cell::default());
        }
    }

    /// 行頭からカーソル位置まで消去
    pub fn erase_line_to_start(&mut self) {
        let row = self.cursor.row;
        for col in 0..=self.cursor.col {
            self.active_grid_mut().set(col, row, Cell::default());
        }
    }

    /// 行全体を消去
    pub fn erase_line(&mut self) {
        let row = self.cursor.row;
        self.active_grid_mut().clear_row(row);
    }

    /// カーソル位置から画面末まで消去
    pub fn erase_display_to_end(&mut self) {
        self.erase_line_to_end();
        let rows = self.active_grid().rows;
        for row in (self.cursor.row + 1)..rows {
            self.active_grid_mut().clear_row(row);
        }
    }

    /// 画面先頭からカーソル位置まで消去
    pub fn erase_display_to_start(&mut self) {
        self.erase_line_to_start();
        for row in 0..self.cursor.row {
            self.active_grid_mut().clear_row(row);
        }
    }

    /// 画面全体を消去
    pub fn erase_display(&mut self) {
        self.active_grid_mut().clear();
    }

    // ───────────────────────────────────────────────────────────────────────
    // モード操作
    // ───────────────────────────────────────────────────────────────────────

    /// 代替スクリーンに切り替え
    pub fn enter_alt_screen(&mut self) {
        if !self.mode.contains(TerminalMode::ALT_SCREEN) {
            self.mode.insert(TerminalMode::ALT_SCREEN);
            self.alt_grid.clear();
            self.save_cursor();
        }
    }

    /// メインスクリーンに切り替え
    pub fn exit_alt_screen(&mut self) {
        if self.mode.contains(TerminalMode::ALT_SCREEN) {
            self.mode.remove(TerminalMode::ALT_SCREEN);
            self.restore_cursor();
            // スクロール領域を全画面にリセット
            self.scroll_top = 0;
            self.scroll_bottom = self.grid.rows.saturating_sub(1);
            // メイン画面を再描画するためにダーティにする
            self.grid.mark_all_dirty();
        }
    }

    /// 選択されたテキストを取得
    pub fn get_selected_text(&self) -> Option<String> {
        if !self.selection.has_selection() {
            return None;
        }

        let (start, end) = match (self.selection.start, self.selection.end) {
            (Some(s), Some(e)) => {
                // 開始と終了を正規化
                if s.1 < e.1 || (s.1 == e.1 && s.0 <= e.0) {
                    (s, e)
                } else {
                    (e, s)
                }
            }
            _ => return None,
        };

        let grid = self.active_grid();
        let mut text = String::new();

        for row in start.1..=end.1 {
            if row >= grid.rows {
                break;
            }

            let col_start = if row == start.1 { start.0 } else { 0 };
            let col_end = if row == end.1 { end.0 } else { grid.cols.saturating_sub(1) };

            for col in col_start..=col_end {
                if col >= grid.cols {
                    break;
                }
                let cell = &grid[(col, row)];
                if cell.character != '\0' {
                    text.push(cell.character);
                }
            }

            // 行末で改行を追加（最後の行以外）
            if row < end.1 {
                text.push('\n');
            }
        }

        // 末尾の空白を削除
        let trimmed = text.trim_end().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// サイズを変更
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
        self.alt_grid.resize(cols, rows);
        self.scroll_bottom = rows - 1;

        // カーソル位置を調整
        if self.cursor.col >= cols {
            self.cursor.col = cols - 1;
        }
        if self.cursor.row >= rows {
            self.cursor.row = rows - 1;
        }

        // タブストップを再計算
        self.tabs.clear();
        for i in (8..cols).step_by(8) {
            self.tabs.push(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_char() {
        let mut term = Terminal::new(80, 24);
        term.input_char('H');
        term.input_char('i');

        assert_eq!(term.grid[(0, 0)].character, 'H');
        assert_eq!(term.grid[(1, 0)].character, 'i');
        assert_eq!(term.cursor.col, 2);
    }

    #[test]
    fn test_newline() {
        let mut term = Terminal::new(80, 24);
        term.input_char('A');
        term.linefeed();
        term.carriage_return();
        term.input_char('B');

        assert_eq!(term.grid[(0, 0)].character, 'A');
        assert_eq!(term.grid[(0, 1)].character, 'B');
    }

    #[test]
    fn test_scroll() {
        let mut term = Terminal::new(80, 3);
        term.scroll_bottom = 2;

        term.input_char('1');
        term.linefeed();
        term.carriage_return();
        term.input_char('2');
        term.linefeed();
        term.carriage_return();
        term.input_char('3');
        term.linefeed();
        term.carriage_return();
        term.input_char('4');

        // スクロール後、最初の'1'は消えているはず
        assert_eq!(term.grid[(0, 0)].character, '2');
    }
}
