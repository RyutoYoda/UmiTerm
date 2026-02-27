//! ペイン管理モジュール
//!
//! ウィンドウ内の画面分割を管理

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use parking_lot::Mutex;

use crate::parser::AnsiParser;
use crate::pty::Pty;
use crate::terminal::Terminal;

// ═══════════════════════════════════════════════════════════════════════════
// ペインID
// ═══════════════════════════════════════════════════════════════════════════

/// ペインの一意識別子
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

impl PaneId {
    /// 新しいペインIDを生成
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 矩形領域
// ═══════════════════════════════════════════════════════════════════════════

/// 正規化された矩形領域（0.0〜1.0）
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn full() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        }
    }

    /// 左半分
    pub fn left_half(&self) -> Self {
        Self {
            x: self.x,
            y: self.y,
            width: self.width / 2.0,
            height: self.height,
        }
    }

    /// 右半分
    pub fn right_half(&self) -> Self {
        Self {
            x: self.x + self.width / 2.0,
            y: self.y,
            width: self.width / 2.0,
            height: self.height,
        }
    }

    /// 上半分
    pub fn top_half(&self) -> Self {
        Self {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height / 2.0,
        }
    }

    /// 下半分
    pub fn bottom_half(&self) -> Self {
        Self {
            x: self.x,
            y: self.y + self.height / 2.0,
            width: self.width,
            height: self.height / 2.0,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ペイン
// ═══════════════════════════════════════════════════════════════════════════

/// 個々のペイン（ターミナル + PTY）
pub struct Pane {
    /// ペインID
    pub id: PaneId,
    /// ターミナル状態
    pub terminal: Arc<Mutex<Terminal>>,
    /// ANSI パーサー
    pub parser: AnsiParser,
    /// PTY（擬似端末）
    pub pty: Pty,
    /// 最後のフレーム時刻
    pub last_frame: Instant,
    /// 最後に出力があった時刻
    pub last_output: Instant,
    /// 再描画が必要か（ダーティフラグ）
    pub dirty: bool,
}

impl Pane {
    /// 新しいペインを作成
    pub fn new(cols: u16, rows: u16) -> Result<Self> {
        let terminal = Arc::new(Mutex::new(Terminal::new(cols as usize, rows as usize)));
        let pty = Pty::spawn(cols, rows, None)?;
        let now = Instant::now();

        Ok(Self {
            id: PaneId::new(),
            terminal,
            parser: AnsiParser::new(),
            pty,
            last_frame: now,
            last_output: now,
            dirty: true, // 初期状態は描画が必要
        })
    }

    /// フレームを更新（PTYからの出力を読み取り）
    /// 戻り値: 出力があったかどうか
    pub fn update(&mut self) -> bool {
        if let Some(data) = self.pty.read() {
            let mut terminal = self.terminal.lock();
            self.parser.process(&mut terminal, &data);

            // DSR等の応答があればPTYに送信
            if let Some(response) = terminal.take_response() {
                let _ = self.pty.write(&response);
            }

            self.last_output = Instant::now();
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// アイドル状態かどうか（指定時間出力がない）
    #[inline]
    pub fn is_idle(&self, idle_threshold_ms: u64) -> bool {
        self.last_output.elapsed().as_millis() > idle_threshold_ms as u128
    }

    /// ダーティフラグをクリア
    #[inline]
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// リサイズ
    pub fn resize(&mut self, cols: u16, rows: u16) {
        {
            let mut terminal = self.terminal.lock();
            terminal.resize(cols as usize, rows as usize);
        }
        let _ = self.pty.resize(cols, rows);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ペインレイアウト
// ═══════════════════════════════════════════════════════════════════════════

/// ペインのレイアウト（再帰的な木構造）
pub enum PaneLayout {
    /// 単一ペイン
    Single(PaneId),
    /// 水平分割（左右）
    HSplit {
        left: Box<PaneLayout>,
        right: Box<PaneLayout>,
        ratio: f32, // 左側の比率（0.0〜1.0）
    },
    /// 垂直分割（上下）
    VSplit {
        top: Box<PaneLayout>,
        bottom: Box<PaneLayout>,
        ratio: f32, // 上側の比率（0.0〜1.0）
    },
}

impl PaneLayout {
    /// 単一ペインのレイアウトを作成
    pub fn single(id: PaneId) -> Self {
        Self::Single(id)
    }

    /// 指定したペインを水平分割（左右）
    pub fn split_horizontal(&mut self, target_id: PaneId, new_id: PaneId) -> bool {
        match self {
            PaneLayout::Single(id) if *id == target_id => {
                *self = PaneLayout::HSplit {
                    left: Box::new(PaneLayout::Single(target_id)),
                    right: Box::new(PaneLayout::Single(new_id)),
                    ratio: 0.5,
                };
                true
            }
            PaneLayout::HSplit { left, right, .. } => {
                left.split_horizontal(target_id, new_id)
                    || right.split_horizontal(target_id, new_id)
            }
            PaneLayout::VSplit { top, bottom, .. } => {
                top.split_horizontal(target_id, new_id)
                    || bottom.split_horizontal(target_id, new_id)
            }
            _ => false,
        }
    }

    /// 指定したペインを垂直分割（上下）
    pub fn split_vertical(&mut self, target_id: PaneId, new_id: PaneId) -> bool {
        match self {
            PaneLayout::Single(id) if *id == target_id => {
                *self = PaneLayout::VSplit {
                    top: Box::new(PaneLayout::Single(target_id)),
                    bottom: Box::new(PaneLayout::Single(new_id)),
                    ratio: 0.5,
                };
                true
            }
            PaneLayout::HSplit { left, right, .. } => {
                left.split_vertical(target_id, new_id)
                    || right.split_vertical(target_id, new_id)
            }
            PaneLayout::VSplit { top, bottom, .. } => {
                top.split_vertical(target_id, new_id)
                    || bottom.split_vertical(target_id, new_id)
            }
            _ => false,
        }
    }

    /// 指定したペインを削除し、レイアウトを再構成
    /// 戻り値: (削除成功, 残りのレイアウト)
    pub fn remove_pane(&mut self, target_id: PaneId) -> Option<PaneLayout> {
        match self {
            PaneLayout::Single(id) if *id == target_id => {
                // 自分自身が削除対象 → 親が処理
                None
            }
            PaneLayout::HSplit { left, right, .. } => {
                if let PaneLayout::Single(id) = left.as_ref() {
                    if *id == target_id {
                        return Some(std::mem::replace(right.as_mut(), PaneLayout::Single(PaneId(0))));
                    }
                }
                if let PaneLayout::Single(id) = right.as_ref() {
                    if *id == target_id {
                        return Some(std::mem::replace(left.as_mut(), PaneLayout::Single(PaneId(0))));
                    }
                }
                if let Some(new_left) = left.remove_pane(target_id) {
                    *left = Box::new(new_left);
                    return Some(std::mem::replace(self, PaneLayout::Single(PaneId(0))));
                }
                if let Some(new_right) = right.remove_pane(target_id) {
                    *right = Box::new(new_right);
                    return Some(std::mem::replace(self, PaneLayout::Single(PaneId(0))));
                }
                None
            }
            PaneLayout::VSplit { top, bottom, .. } => {
                if let PaneLayout::Single(id) = top.as_ref() {
                    if *id == target_id {
                        return Some(std::mem::replace(bottom.as_mut(), PaneLayout::Single(PaneId(0))));
                    }
                }
                if let PaneLayout::Single(id) = bottom.as_ref() {
                    if *id == target_id {
                        return Some(std::mem::replace(top.as_mut(), PaneLayout::Single(PaneId(0))));
                    }
                }
                if let Some(new_top) = top.remove_pane(target_id) {
                    *top = Box::new(new_top);
                    return Some(std::mem::replace(self, PaneLayout::Single(PaneId(0))));
                }
                if let Some(new_bottom) = bottom.remove_pane(target_id) {
                    *bottom = Box::new(new_bottom);
                    return Some(std::mem::replace(self, PaneLayout::Single(PaneId(0))));
                }
                None
            }
            _ => None,
        }
    }

    /// 各ペインの矩形領域を計算
    pub fn calculate_rects(&self, bounds: Rect) -> Vec<(PaneId, Rect)> {
        let mut result = Vec::new();
        self.calculate_rects_inner(bounds, &mut result);
        result
    }

    fn calculate_rects_inner(&self, bounds: Rect, result: &mut Vec<(PaneId, Rect)>) {
        match self {
            PaneLayout::Single(id) => {
                result.push((*id, bounds));
            }
            PaneLayout::HSplit { left, right, ratio } => {
                let left_bounds = Rect {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width * ratio,
                    height: bounds.height,
                };
                let right_bounds = Rect {
                    x: bounds.x + bounds.width * ratio,
                    y: bounds.y,
                    width: bounds.width * (1.0 - ratio),
                    height: bounds.height,
                };
                left.calculate_rects_inner(left_bounds, result);
                right.calculate_rects_inner(right_bounds, result);
            }
            PaneLayout::VSplit { top, bottom, ratio } => {
                let top_bounds = Rect {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height * ratio,
                };
                let bottom_bounds = Rect {
                    x: bounds.x,
                    y: bounds.y + bounds.height * ratio,
                    width: bounds.width,
                    height: bounds.height * (1.0 - ratio),
                };
                top.calculate_rects_inner(top_bounds, result);
                bottom.calculate_rects_inner(bottom_bounds, result);
            }
        }
    }

    /// すべてのペインIDを取得
    pub fn all_pane_ids(&self) -> Vec<PaneId> {
        let mut result = Vec::new();
        self.collect_pane_ids(&mut result);
        result
    }

    fn collect_pane_ids(&self, result: &mut Vec<PaneId>) {
        match self {
            PaneLayout::Single(id) => {
                result.push(*id);
            }
            PaneLayout::HSplit { left, right, .. } => {
                left.collect_pane_ids(result);
                right.collect_pane_ids(result);
            }
            PaneLayout::VSplit { top, bottom, .. } => {
                top.collect_pane_ids(result);
                bottom.collect_pane_ids(result);
            }
        }
    }

    /// 次のペインIDを取得（フォーカス移動用）
    pub fn next_pane(&self, current: PaneId) -> Option<PaneId> {
        let ids = self.all_pane_ids();
        if ids.len() <= 1 {
            return None;
        }
        let current_idx = ids.iter().position(|&id| id == current)?;
        let next_idx = (current_idx + 1) % ids.len();
        Some(ids[next_idx])
    }

    /// 前のペインIDを取得（フォーカス移動用）
    pub fn prev_pane(&self, current: PaneId) -> Option<PaneId> {
        let ids = self.all_pane_ids();
        if ids.len() <= 1 {
            return None;
        }
        let current_idx = ids.iter().position(|&id| id == current)?;
        let prev_idx = if current_idx == 0 {
            ids.len() - 1
        } else {
            current_idx - 1
        };
        Some(ids[prev_idx])
    }

    /// ペイン数を取得
    pub fn pane_count(&self) -> usize {
        self.all_pane_ids().len()
    }

    /// 指定した正規化座標にあるペインIDを取得
    pub fn pane_at(&self, x: f32, y: f32, bounds: Rect) -> Option<PaneId> {
        match self {
            PaneLayout::Single(id) => {
                if x >= bounds.x && x < bounds.x + bounds.width
                    && y >= bounds.y && y < bounds.y + bounds.height
                {
                    Some(*id)
                } else {
                    None
                }
            }
            PaneLayout::HSplit { left, right, ratio } => {
                let split_x = bounds.x + bounds.width * ratio;
                if x < split_x {
                    let left_bounds = Rect {
                        x: bounds.x,
                        y: bounds.y,
                        width: bounds.width * ratio,
                        height: bounds.height,
                    };
                    left.pane_at(x, y, left_bounds)
                } else {
                    let right_bounds = Rect {
                        x: split_x,
                        y: bounds.y,
                        width: bounds.width * (1.0 - ratio),
                        height: bounds.height,
                    };
                    right.pane_at(x, y, right_bounds)
                }
            }
            PaneLayout::VSplit { top, bottom, ratio } => {
                let split_y = bounds.y + bounds.height * ratio;
                if y < split_y {
                    let top_bounds = Rect {
                        x: bounds.x,
                        y: bounds.y,
                        width: bounds.width,
                        height: bounds.height * ratio,
                    };
                    top.pane_at(x, y, top_bounds)
                } else {
                    let bottom_bounds = Rect {
                        x: bounds.x,
                        y: split_y,
                        width: bounds.width,
                        height: bounds.height * (1.0 - ratio),
                    };
                    bottom.pane_at(x, y, bottom_bounds)
                }
            }
        }
    }

    /// 指定した座標が境界線上にあるかチェック
    /// 戻り値: Some((境界線の種類, 境界線の位置情報))
    pub fn border_at(&self, x: f32, y: f32, bounds: Rect, threshold: f32) -> Option<BorderHit> {
        match self {
            PaneLayout::Single(_) => None,
            PaneLayout::HSplit { left, right, ratio } => {
                let split_x = bounds.x + bounds.width * ratio;
                // 境界線の判定
                if (x - split_x).abs() < threshold && y >= bounds.y && y < bounds.y + bounds.height {
                    return Some(BorderHit::Vertical {
                        x: split_x,
                        y_start: bounds.y,
                        y_end: bounds.y + bounds.height,
                        layout_path: vec![],
                    });
                }
                // 子レイアウトを再帰的にチェック
                let left_bounds = Rect {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width * ratio,
                    height: bounds.height,
                };
                if let Some(mut hit) = left.border_at(x, y, left_bounds, threshold) {
                    hit.push_path(BorderDirection::Left);
                    return Some(hit);
                }
                let right_bounds = Rect {
                    x: split_x,
                    y: bounds.y,
                    width: bounds.width * (1.0 - ratio),
                    height: bounds.height,
                };
                if let Some(mut hit) = right.border_at(x, y, right_bounds, threshold) {
                    hit.push_path(BorderDirection::Right);
                    return Some(hit);
                }
                None
            }
            PaneLayout::VSplit { top, bottom, ratio } => {
                let split_y = bounds.y + bounds.height * ratio;
                // 境界線の判定
                if (y - split_y).abs() < threshold && x >= bounds.x && x < bounds.x + bounds.width {
                    return Some(BorderHit::Horizontal {
                        y: split_y,
                        x_start: bounds.x,
                        x_end: bounds.x + bounds.width,
                        layout_path: vec![],
                    });
                }
                // 子レイアウトを再帰的にチェック
                let top_bounds = Rect {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height * ratio,
                };
                if let Some(mut hit) = top.border_at(x, y, top_bounds, threshold) {
                    hit.push_path(BorderDirection::Top);
                    return Some(hit);
                }
                let bottom_bounds = Rect {
                    x: bounds.x,
                    y: split_y,
                    width: bounds.width,
                    height: bounds.height * (1.0 - ratio),
                };
                if let Some(mut hit) = bottom.border_at(x, y, bottom_bounds, threshold) {
                    hit.push_path(BorderDirection::Bottom);
                    return Some(hit);
                }
                None
            }
        }
    }

    /// パスを使って比率を更新
    pub fn update_ratio(&mut self, path: &[BorderDirection], new_ratio: f32) {
        if path.is_empty() {
            // このノードの比率を更新
            match self {
                PaneLayout::HSplit { ratio, .. } | PaneLayout::VSplit { ratio, .. } => {
                    *ratio = new_ratio.clamp(0.1, 0.9);
                }
                _ => {}
            }
        } else {
            // 子に委譲
            match self {
                PaneLayout::HSplit { left, right, .. } => {
                    match path[0] {
                        BorderDirection::Left => left.update_ratio(&path[1..], new_ratio),
                        BorderDirection::Right => right.update_ratio(&path[1..], new_ratio),
                        _ => {}
                    }
                }
                PaneLayout::VSplit { top, bottom, .. } => {
                    match path[0] {
                        BorderDirection::Top => top.update_ratio(&path[1..], new_ratio),
                        BorderDirection::Bottom => bottom.update_ratio(&path[1..], new_ratio),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

/// 境界線の方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderDirection {
    Left,
    Right,
    Top,
    Bottom,
}

/// 境界線のヒット情報
#[derive(Debug, Clone)]
pub enum BorderHit {
    /// 垂直境界線（左右分割の境界）
    Vertical {
        x: f32,
        y_start: f32,
        y_end: f32,
        layout_path: Vec<BorderDirection>,
    },
    /// 水平境界線（上下分割の境界）
    Horizontal {
        y: f32,
        x_start: f32,
        x_end: f32,
        layout_path: Vec<BorderDirection>,
    },
}

impl BorderHit {
    pub fn push_path(&mut self, dir: BorderDirection) {
        match self {
            BorderHit::Vertical { layout_path, .. } | BorderHit::Horizontal { layout_path, .. } => {
                layout_path.insert(0, dir);
            }
        }
    }

    pub fn path(&self) -> &[BorderDirection] {
        match self {
            BorderHit::Vertical { layout_path, .. } | BorderHit::Horizontal { layout_path, .. } => {
                layout_path
            }
        }
    }

    pub fn is_vertical(&self) -> bool {
        matches!(self, BorderHit::Vertical { .. })
    }
}
