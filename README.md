# UmiTerm 🌊

<img src="https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white" /> <img src="https://img.shields.io/badge/wgpu-4285F4?style=flat&logo=webgpu&logoColor=white" /> <img src="https://img.shields.io/badge/macOS-000000?style=flat&logo=apple&logoColor=white" />

Rust製GPU加速ターミナルエミュレータ

<img width="914" height="268" alt="image" src="https://github.com/user-attachments/assets/613fee04-cfa5-432b-aeda-9011585f1d9c" />

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

```mermaid
graph TB
    subgraph UmiTerm
        subgraph Input["入力層"]
            W[winit<br/>Window/Event]
            K[Keyboard]
            IME[IME<br/>日本語入力]
        end

        subgraph Core["コア層"]
            M[main.rs<br/>イベントループ]
            T[Terminal<br/>状態管理]
            P[Parser<br/>ANSI解析]
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
    W --> M
    M --> T
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
| `main.rs` | エントリーポイント | winitウィンドウ、イベントループ、IME処理 |
| `pty.rs` | 擬似端末 | シェル通信、ノンブロッキングI/O |
| `terminal.rs` | ターミナル状態 | カーソル、スクロール、スタイル管理 |
| `grid.rs` | 文字バッファ | 2Dセル配列、ダーティフラグ |
| `parser.rs` | ANSIパーサー | CSI/OSC/SGRシーケンス解析 |
| `renderer.rs` | GPUレンダラー | wgpu描画、グリフキャッシュ |
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

## 対応機能

- [x] 基本的な文字表示
- [x] 256色/TrueColor
- [x] カーソル移動・形状変更
- [x] スクロール
- [x] 代替スクリーン（vim対応）
- [x] 太字/斜体/下線
- [x] 日本語入力（IME対応）
- [x] 全角文字表示
