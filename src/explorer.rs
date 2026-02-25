//! ファイルエクスプローラー（サイドバー）
//!
//! IDEライクなファイルツリーをターミナルに統合

use std::fs;
use std::path::{Path, PathBuf};

/// ファイルエントリの種類
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    Directory,
    File,
}

/// ファイルツリーのエントリ
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub depth: usize,
    pub expanded: bool,
    pub children_loaded: bool,
}

impl FileEntry {
    pub fn new(path: PathBuf, depth: usize) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        let kind = if path.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };

        Self {
            name,
            path,
            kind,
            depth,
            expanded: false,
            children_loaded: false,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

/// ファイルエクスプローラーの状態
pub struct Explorer {
    /// ルートディレクトリ
    pub root: PathBuf,
    /// 表示するエントリ（フラット化されたツリー）
    pub entries: Vec<FileEntry>,
    /// 選択中のインデックス
    pub selected: usize,
    /// サイドバーの幅（文字数）
    pub width: usize,
    /// 表示中かどうか
    pub visible: bool,
    /// スクロールオフセット
    pub scroll_offset: usize,
}

impl Explorer {
    /// 新しいエクスプローラーを作成
    pub fn new(root: PathBuf) -> Self {
        let mut explorer = Self {
            root: root.clone(),
            entries: Vec::new(),
            selected: 0,
            width: 25,
            visible: false,
            scroll_offset: 0,
        };
        explorer.load_directory(&root, 0);
        explorer
    }

    /// ディレクトリを読み込んでエントリに追加
    fn load_directory(&mut self, path: &Path, depth: usize) {
        if let Ok(read_dir) = fs::read_dir(path) {
            let mut entries: Vec<FileEntry> = read_dir
                .filter_map(|e| e.ok())
                .filter(|e| {
                    // 隠しファイルを除外（.で始まるもの）
                    let name = e.file_name();
                    let name_str = name.to_string_lossy();
                    !name_str.starts_with('.')
                })
                .map(|e| FileEntry::new(e.path(), depth))
                .collect();

            // ディレクトリを先に、その後ファイルをアルファベット順
            entries.sort_by(|a, b| {
                match (&a.kind, &b.kind) {
                    (EntryKind::Directory, EntryKind::File) => std::cmp::Ordering::Less,
                    (EntryKind::File, EntryKind::Directory) => std::cmp::Ordering::Greater,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                }
            });

            self.entries = entries;
        }
    }

    /// 表示/非表示を切り替え
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// 上に移動
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.ensure_visible();
        }
    }

    /// 下に移動
    pub fn move_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
            self.ensure_visible();
        }
    }

    /// 選択中のエントリを取得
    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.entries.get(self.selected)
    }

    /// ディレクトリを展開/折りたたみ
    pub fn toggle_expand(&mut self) {
        if let Some(entry) = self.entries.get(self.selected).cloned() {
            if entry.is_dir() {
                if entry.expanded {
                    // 折りたたむ: 子エントリを削除
                    self.collapse_at(self.selected);
                } else {
                    // 展開: 子エントリを挿入
                    self.expand_at(self.selected);
                }
            }
        }
    }

    /// 指定位置のディレクトリを展開
    fn expand_at(&mut self, index: usize) {
        if let Some(entry) = self.entries.get_mut(index) {
            if !entry.is_dir() || entry.expanded {
                return;
            }
            entry.expanded = true;
            let path = entry.path.clone();
            let depth = entry.depth + 1;

            // 子エントリを読み込み
            if let Ok(read_dir) = fs::read_dir(&path) {
                let mut children: Vec<FileEntry> = read_dir
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        let name = e.file_name();
                        let name_str = name.to_string_lossy();
                        !name_str.starts_with('.')
                    })
                    .map(|e| FileEntry::new(e.path(), depth))
                    .collect();

                children.sort_by(|a, b| {
                    match (&a.kind, &b.kind) {
                        (EntryKind::Directory, EntryKind::File) => std::cmp::Ordering::Less,
                        (EntryKind::File, EntryKind::Directory) => std::cmp::Ordering::Greater,
                        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    }
                });

                // index + 1 の位置に挿入
                let insert_pos = index + 1;
                for (i, child) in children.into_iter().enumerate() {
                    self.entries.insert(insert_pos + i, child);
                }
            }
        }
    }

    /// 指定位置のディレクトリを折りたたむ
    fn collapse_at(&mut self, index: usize) {
        if let Some(entry) = self.entries.get_mut(index) {
            if !entry.is_dir() || !entry.expanded {
                return;
            }
            entry.expanded = false;
            let depth = entry.depth;

            // 子エントリを削除（深さが大きいものを連続して削除）
            let mut remove_count = 0;
            for i in (index + 1)..self.entries.len() {
                if self.entries[i].depth > depth {
                    remove_count += 1;
                } else {
                    break;
                }
            }
            self.entries.drain((index + 1)..(index + 1 + remove_count));
        }
    }

    /// 選択したエントリのパスを返す（ディレクトリならそのパス、ファイルなら親ディレクトリ）
    pub fn get_cd_path(&self) -> Option<PathBuf> {
        self.selected_entry().map(|entry| {
            if entry.is_dir() {
                entry.path.clone()
            } else {
                entry.path.parent().map(|p| p.to_path_buf()).unwrap_or(entry.path.clone())
            }
        })
    }

    /// スクロール位置を調整して選択が見えるようにする
    fn ensure_visible(&mut self) {
        // 表示可能な行数（仮に20行とする、後でrendererから設定）
        let visible_rows = 20;

        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.selected - visible_rows + 1;
        }
    }

    /// 表示可能行数を設定
    pub fn set_visible_rows(&mut self, rows: usize) {
        let visible_rows = rows.saturating_sub(2); // ヘッダー分
        if self.selected >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.selected.saturating_sub(visible_rows - 1);
        }
    }

    /// ルートディレクトリを変更
    pub fn set_root(&mut self, path: PathBuf) {
        self.root = path.clone();
        self.entries.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.load_directory(&path, 0);
    }
}
