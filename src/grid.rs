//! グリッドモジュール - ターミナルの文字バッファを管理
//!
//! 高速化のポイント:
//! - 連続メモリレイアウト（キャッシュフレンドリー）
//! - ダーティフラグによる差分更新

use std::ops::{Index, IndexMut};

// ═══════════════════════════════════════════════════════════════════════════
// セル（1文字分のデータ）
// ═══════════════════════════════════════════════════════════════════════════

/// ターミナルの1マスを表す構造体
/// サイズを最小限に抑えてキャッシュ効率を上げる
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// 表示する文字（UTF-8の1文字）
    pub character: char,
    /// 前景色（RGBA）
    pub fg: Color,
    /// 背景色（RGBA）
    pub bg: Color,
    /// スタイルフラグ（ボールド、イタリック等）
    pub flags: CellFlags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            character: ' ',
            fg: Color::EMERALD, // エメラルドブルー
            bg: Color::BLACK,
            flags: CellFlags::empty(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// カラー
// ═══════════════════════════════════════════════════════════════════════════

/// RGBA カラー（各チャンネル 8bit）
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255, a: 255 };
    pub const RED: Self = Self { r: 255, g: 0, b: 0, a: 255 };
    pub const GREEN: Self = Self { r: 0, g: 255, b: 0, a: 255 };
    pub const BLUE: Self = Self { r: 0, g: 0, b: 255, a: 255 };
    pub const YELLOW: Self = Self { r: 255, g: 255, b: 0, a: 255 };
    pub const CYAN: Self = Self { r: 0, g: 255, b: 255, a: 255 };
    pub const MAGENTA: Self = Self { r: 255, g: 0, b: 255, a: 255 };
    /// エメラルドブルー（デフォルト文字色）
    pub const EMERALD: Self = Self { r: 80, g: 220, b: 200, a: 255 };

    /// RGB から生成
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// ANSI 256色パレットから変換
    pub fn from_ansi256(code: u8) -> Self {
        match code {
            // 標準16色
            0 => Self::rgb(0, 0, 0),
            1 => Self::rgb(128, 0, 0),
            2 => Self::rgb(0, 128, 0),
            3 => Self::rgb(128, 128, 0),
            4 => Self::rgb(0, 0, 128),
            5 => Self::rgb(128, 0, 128),
            6 => Self::rgb(0, 128, 128),
            7 => Self::rgb(192, 192, 192),
            8 => Self::rgb(128, 128, 128),
            9 => Self::rgb(255, 0, 0),
            10 => Self::rgb(0, 255, 0),
            11 => Self::rgb(255, 255, 0),
            12 => Self::rgb(0, 0, 255),
            13 => Self::rgb(255, 0, 255),
            14 => Self::rgb(0, 255, 255),
            15 => Self::rgb(255, 255, 255),
            // 216色キューブ (16-231)
            16..=231 => {
                let idx = code - 16;
                let r = (idx / 36) % 6;
                let g = (idx / 6) % 6;
                let b = idx % 6;
                // 各成分を 0, 95, 135, 175, 215, 255 にマップ
                let to_rgb = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Self::rgb(to_rgb(r), to_rgb(g), to_rgb(b))
            }
            // グレースケール (232-255)
            232..=255 => {
                let gray = 8 + (code - 232) * 10;
                Self::rgb(gray, gray, gray)
            }
        }
    }

    /// 浮動小数点数の配列に変換（GPU用）
    pub fn to_f32_array(&self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// セルフラグ（ビットフラグで効率的に管理）
// ═══════════════════════════════════════════════════════════════════════════

bitflags::bitflags! {
    /// セルのスタイルフラグ
    #[derive(Clone, Copy, Debug, PartialEq, Default)]
    pub struct CellFlags: u8 {
        const BOLD       = 0b0000_0001;
        const ITALIC     = 0b0000_0010;
        const UNDERLINE  = 0b0000_0100;
        const BLINK      = 0b0000_1000;
        const INVERSE    = 0b0001_0000;
        const HIDDEN     = 0b0010_0000;
        const STRIKEOUT  = 0b0100_0000;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// グリッド（文字バッファ）
// ═══════════════════════════════════════════════════════════════════════════

/// ターミナルのグリッド（2次元の文字バッファ）
/// 連続したメモリに配置してキャッシュ効率を最大化
pub struct Grid {
    /// セルの配列（行優先で格納）
    cells: Vec<Cell>,
    /// 列数
    pub cols: usize,
    /// 行数
    pub rows: usize,
    /// 変更があった行を追跡（差分レンダリング用）
    dirty_lines: Vec<bool>,
}

impl Grid {
    /// 新しいグリッドを作成
    pub fn new(cols: usize, rows: usize) -> Self {
        let total = cols * rows;
        Self {
            cells: vec![Cell::default(); total],
            cols,
            rows,
            dirty_lines: vec![true; rows], // 初期状態は全行ダーティ
        }
    }

    /// グリッドのサイズを変更
    pub fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let mut new_cells = vec![Cell::default(); new_cols * new_rows];

        // 既存のデータをコピー（可能な範囲で）
        let copy_cols = self.cols.min(new_cols);
        let copy_rows = self.rows.min(new_rows);

        for row in 0..copy_rows {
            for col in 0..copy_cols {
                new_cells[row * new_cols + col] = self.cells[row * self.cols + col];
            }
        }

        self.cells = new_cells;
        self.cols = new_cols;
        self.rows = new_rows;
        self.dirty_lines = vec![true; new_rows];
    }

    /// 指定位置のセルを取得
    #[inline]
    pub fn get(&self, col: usize, row: usize) -> Option<&Cell> {
        if col < self.cols && row < self.rows {
            Some(&self.cells[row * self.cols + col])
        } else {
            None
        }
    }

    /// 指定位置のセルを変更可能な参照で取得
    #[inline]
    pub fn get_mut(&mut self, col: usize, row: usize) -> Option<&mut Cell> {
        if col < self.cols && row < self.rows {
            self.dirty_lines[row] = true;
            Some(&mut self.cells[row * self.cols + col])
        } else {
            None
        }
    }

    /// 指定行にセルを設定
    #[inline]
    pub fn set(&mut self, col: usize, row: usize, cell: Cell) {
        if col < self.cols && row < self.rows {
            self.cells[row * self.cols + col] = cell;
            self.dirty_lines[row] = true;
        }
    }

    /// 行をクリア
    pub fn clear_row(&mut self, row: usize) {
        if row < self.rows {
            let start = row * self.cols;
            for i in 0..self.cols {
                self.cells[start + i] = Cell::default();
            }
            self.dirty_lines[row] = true;
        }
    }

    /// グリッド全体をクリア
    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            *cell = Cell::default();
        }
        self.dirty_lines.fill(true);
    }

    /// 行をスクロールアップ（最下行が空になる）
    pub fn scroll_up(&mut self, amount: usize) {
        if amount >= self.rows {
            self.clear();
            return;
        }

        // メモリコピーで高速にスクロール
        let shift = amount * self.cols;
        self.cells.copy_within(shift.., 0);

        // 新しい行をクリア
        let clear_start = (self.rows - amount) * self.cols;
        for i in clear_start..self.cells.len() {
            self.cells[i] = Cell::default();
        }

        self.dirty_lines.fill(true);
    }

    /// ダーティフラグをチェック
    pub fn is_dirty(&self, row: usize) -> bool {
        self.dirty_lines.get(row).copied().unwrap_or(false)
    }

    /// ダーティフラグをクリア
    pub fn clear_dirty(&mut self) {
        self.dirty_lines.fill(false);
    }

    /// 全行をダーティにする
    pub fn mark_all_dirty(&mut self) {
        self.dirty_lines.fill(true);
    }

    /// 行全体のスライスを取得（高速なレンダリング用）
    pub fn row_slice(&self, row: usize) -> &[Cell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }
}

// インデックスアクセスを実装（grid[(col, row)] でアクセス可能に）
impl Index<(usize, usize)> for Grid {
    type Output = Cell;

    fn index(&self, (col, row): (usize, usize)) -> &Self::Output {
        &self.cells[row * self.cols + col]
    }
}

impl IndexMut<(usize, usize)> for Grid {
    fn index_mut(&mut self, (col, row): (usize, usize)) -> &mut Self::Output {
        self.dirty_lines[row] = true;
        &mut self.cells[row * self.cols + col]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_basic() {
        let mut grid = Grid::new(80, 24);

        // セルの設定とテスト
        grid.set(0, 0, Cell {
            character: 'A',
            ..Default::default()
        });

        assert_eq!(grid[(0, 0)].character, 'A');
    }

    #[test]
    fn test_scroll() {
        let mut grid = Grid::new(80, 3);
        grid.set(0, 0, Cell { character: 'A', ..Default::default() });
        grid.set(0, 1, Cell { character: 'B', ..Default::default() });
        grid.set(0, 2, Cell { character: 'C', ..Default::default() });

        grid.scroll_up(1);

        assert_eq!(grid[(0, 0)].character, 'B');
        assert_eq!(grid[(0, 1)].character, 'C');
        assert_eq!(grid[(0, 2)].character, ' ');
    }
}
