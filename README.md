# UmiTerm 🌊

<img src="https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white" /> <img src="https://img.shields.io/badge/wgpu-4285F4?style=flat&logo=webgpu&logoColor=white" /> <img src="https://img.shields.io/badge/macOS-000000?style=flat&logo=apple&logoColor=white" />

Rust製GPU加速ターミナルエミュレータ

**[ベンチマーク結果](BENCHMARK.md)** - Alacrittyと同クラスの性能、バイナリサイズ1/3

<img width="731" height="198" alt="スクリーンショット 2026-02-25 11 20 13" src="https://github.com/user-attachments/assets/35f47129-aade-480e-900e-709d3ac9dd61" />

## インストール

### Homebrew（推奨）
```bash
brew tap ryutoyoda/tap
brew install --cask umiterm
```

### 手動インストール
1. [Releases](https://github.com/RyutoYoda/UmiTerm/releases) から `UmiTerm-v*.zip` をダウンロード
2. 解凍して `UmiTerm.app` を `/Applications` にドラッグ
3. 初回起動時は右クリック →「開く」を選択

## アーキテクチャ

```
┌─────────────────────────────────────────────────────────────┐
│                         UmiTerm                             │
├─────────────────────────────────────────────────────────────┤
│  App                                                        │
│  └─ windows: HashMap<WindowId, WindowState>                 │
│       └─ WindowState                                        │
│            ├─ window: Arc<Window>     (winit)               │
│            ├─ renderer: Renderer      (wgpu GPU描画)        │
│            ├─ layout: PaneLayout      (分割レイアウト)       │
│            └─ panes: HashMap<PaneId, Pane>                  │
│                 └─ Pane                                     │
│                      ├─ terminal: Terminal (状態管理)        │
│                      ├─ parser: AnsiParser (ANSI解析)       │
│                      └─ pty: Pty           (擬似端末)        │
└─────────────────────────────────────────────────────────────┘
```

```mermaid
graph TB
    subgraph UmiTerm
        subgraph Input["入力層"]
            W[winit<br/>Window/Event]
            K[Keyboard]
            IME[IME<br/>日本語入力]
            Mouse[Mouse<br/>クリック/ドラッグ]
        end

        subgraph Core["コア層"]
            M[main.rs<br/>イベントループ]
            T[Terminal<br/>状態管理]
            P[Parser<br/>ANSI解析]
            Pane[Pane<br/>ペイン管理]
        end

        subgraph IO["I/O層"]
            PTY[PTY<br/>擬似端末]
            SH[Shell<br/>bash/zsh]
        end

        subgraph Render["描画層"]
            G[Grid<br/>文字バッファ]
            R[Renderer<br/>GPU描画]
            S[Shader<br/>wgsl]
        end
    end

    K --> W
    IME --> W
    Mouse --> W
    W --> M
    M --> Pane
    Pane --> T
    M --> R
    T --> G
    T <--> PTY
    PTY <--> SH
    P --> T
    G --> R
    R --> S

    style W fill:#50dcc8,stroke:#333,color:#000
    style T fill:#50dcc8,stroke:#333,color:#000
    style R fill:#50dcc8,stroke:#333,color:#000
    style G fill:#3cb4a0,stroke:#333,color:#000
    style Pane fill:#50dcc8,stroke:#333,color:#000
```

## データフロー

```mermaid
sequenceDiagram
    participant User as ユーザー
    participant W as winit
    participant PTY as PTY
    participant Shell as Shell
    participant T as Terminal
    participant P as Parser
    participant R as Renderer

    User->>W: キー入力
    W->>PTY: バイト送信
    PTY->>Shell: 入力転送
    Shell->>PTY: 出力
    PTY->>P: ANSIシーケンス
    P->>T: 状態更新
    T->>R: Grid
    R->>User: GPU描画
```

## 各モジュールの役割

| モジュール | 役割 | 主な機能 |
|-----------|------|----------|
| `main.rs` | エントリーポイント | winitウィンドウ、イベントループ、IME処理、マウス処理 |
| `pane.rs` | ペイン管理 | 画面分割、レイアウト、境界線ドラッグ |
| `pty.rs` | 擬似端末 | シェル通信、ノンブロッキングI/O |
| `terminal.rs` | ターミナル状態 | カーソル、スクロール、スタイル管理 |
| `grid.rs` | 文字バッファ | 2Dセル配列、ダーティフラグ |
| `parser.rs` | ANSIパーサー | CSI/OSC/SGRシーケンス解析 |
| `renderer.rs` | GPUレンダラー | wgpu描画、グリフキャッシュ、ペイン描画 |
| `shader.wgsl` | シェーダー | 背景・テキスト描画 |

## ビルド・実行

```bash
# 開発版
cargo run

# リリース版（最適化済み）
cargo run --release

# カスタムフォント
UMITERM_FONT=/path/to/font.ttf cargo run --release
```

## 依存クレート

| クレート | 用途 |
|---------|------|
| wgpu | GPU描画 |
| winit | ウィンドウ管理 |
| portable-pty | 擬似端末 |
| vte | ANSIパーサー |
| fontdue | フォントラスタライズ |
| crossbeam-channel | スレッド間通信 |
| parking_lot | 高速ロック |
| unicode-width | 全角文字幅計算 |

## キーバインド

### ウィンドウ操作

| キー | 機能 |
|------|------|
| `Cmd + N` | 新規ウィンドウを開く |
| `Cmd + W` | 現在のペインを閉じる（最後の1つならウィンドウを閉じる） |

### ペイン操作（画面分割）

| キー | 機能 |
|------|------|
| `Cmd + D` | 縦分割（左右に分割） |
| `Cmd + Shift + D` | 横分割（上下に分割） |
| `Cmd + ]` | 次のペインにフォーカス移動 |
| `Cmd + [` | 前のペインにフォーカス移動 |

### マウス操作

| 操作 | 機能 |
|------|------|
| **クリック** | クリックしたペインにフォーカスを切り替え |
| **ドラッグ** | 境界線をドラッグしてペインサイズを調整 |

※ 境界線にマウスを合わせるとカーソルがリサイズカーソル（↔ / ↕）に変わります

### ターミナル操作

| キー | 機能 |
|------|------|
| `Ctrl + C` | 実行中のプロセスを中断 |
| `Ctrl + D` | EOF（シェル終了） |
| `Ctrl + Z` | プロセスを一時停止 |
| `Ctrl + L` | 画面クリア |
| `Ctrl + A` | 行頭へ移動 |
| `Ctrl + E` | 行末へ移動 |
| `Ctrl + U` | カーソルより前を削除 |
| `Ctrl + K` | カーソルより後を削除 |
| `Ctrl + W` | 単語を削除 |
| `Ctrl + R` | 履歴検索 |

## 対応機能

- [x] 基本的な文字表示
- [x] 256色/TrueColor
- [x] カーソル移動・形状変更
- [x] スクロール
- [x] 代替スクリーン（vim対応）
- [x] 太字/斜体/下線
- [x] 日本語入力（IME対応）
- [x] 全角文字表示
- [x] マルチウィンドウ
- [x] 画面分割（ペイン）
- [x] マウスでペイン切り替え
- [x] ドラッグでペインサイズ調整
