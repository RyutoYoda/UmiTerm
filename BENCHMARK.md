# UmiTerm ベンチマーク

Alacritty（最速ターミナルの定番）との比較ベンチマーク結果。

**テスト環境:**
- macOS (Apple Silicon / arm64)
- 2026年2月25日測定

## 結果サマリー

| 項目 | UmiTerm | Alacritty | 勝者 |
|------|---------|-----------|------|
| **バイナリサイズ** | **4.3 MB** | 14 MB | UmiTerm |
| **起動時間（5回平均）** | 0.190秒 | **0.171秒** | Alacritty |
| **メモリ使用量（RSS）** | 88 MB | **65 MB** | Alacritty |

## 詳細

### バイナリサイズ

```
UmiTerm:   4.3 MB
Alacritty: 14 MB
```

UmiTermはAlacrittyの **約1/3** のサイズ。

### 起動時間（5回平均）

```
UmiTerm:
  Run 1: 0.170秒
  Run 2: 0.181秒
  Run 3: 0.195秒
  Run 4: 0.201秒
  Run 5: 0.203秒
  平均: 0.190秒

Alacritty:
  Run 1: 0.164秒
  Run 2: 0.178秒
  Run 3: 0.162秒
  Run 4: 0.182秒
  Run 5: 0.170秒
  平均: 0.171秒
```

差は約10%。両者とも0.2秒以下の高速起動。

### メモリ使用量（RSS）

```
UmiTerm:   88.2 MB
Alacritty: 65.2 MB
```

UmiTermはGPUバッファとグリフキャッシュの最適化で改善の余地あり。

## 技術比較

| 項目 | UmiTerm | Alacritty |
|------|---------|-----------|
| 言語 | Rust | Rust |
| GPU API | wgpu (Metal/Vulkan/DX12) | OpenGL |
| ANSIパーサー | vte | vte |
| フォントレンダリング | fontdue | crossfont |
| コード行数 | ~2,500行 | ~30,000行 |

## 結論

- **UmiTermはAlacrittyと同クラスの性能**
- バイナリサイズで大幅に勝る（1/3）
- wgpu採用で将来性あり（WebGPU対応など）
- シンプルなコードベースで拡張しやすい

## 自分でベンチマークを実行

```bash
# スループットテスト（各ターミナルで実行）
time seq 1 100000

# カラー出力テスト
time for i in $(seq 1 10000); do
    echo -e "\033[31mRed\033[32mGreen\033[34mBlue\033[0m $i"
done
```
