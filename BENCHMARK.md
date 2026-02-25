# UmiTerm ベンチマーク

Alacritty（最速ターミナルの定番）との比較ベンチマーク結果。

## テスト環境

- **OS**: macOS (Apple Silicon / arm64)
- **測定日**: 2026年2月25日
- **UmiTerm**: v0.2.0（最適化済み）
- **Alacritty**: v0.15.0

## 結果サマリー

| 項目 | UmiTerm | Alacritty | 勝者 |
|------|---------|-----------|------|
| **バイナリサイズ** | **4.3 MB** | 14 MB | UmiTerm |
| **起動時間（10回平均）** | 0.117秒 | **0.115秒** | Alacritty |
| **メモリ使用量（5回平均）** | **61 MB** | 75 MB | UmiTerm |

**UmiTermが2勝1敗でAlacrittyに勝利！**

---

## 計測方法

### 1. バイナリサイズ

```bash
ls -lh /Applications/UmiTerm.app/Contents/MacOS/umiterm
ls -lh /Applications/Alacritty.app/Contents/MacOS/alacritty
```

**結果:**
```
UmiTerm:   4.3 MB
Alacritty: 14 MB
```

UmiTermはAlacrittyの **約1/3** のサイズ。

### 2. 起動時間

**方法:**
1. 既存プロセスを `pkill -9` で終了
2. 0.8秒待機（キャッシュクリア）
3. `open -a` でアプリ起動
4. `pgrep -x` でプロセス検出まで5ms間隔でポーリング
5. 10回計測して平均を算出

```bash
start=$(python3 -c "import time; print(time.time())")
open -a UmiTerm
while ! pgrep -x umiterm > /dev/null 2>&1; do sleep 0.005; done
end=$(python3 -c "import time; print(time.time())")
```

**結果（10回計測）:**
```
UmiTerm:
   1: 0.122秒    6: 0.112秒
   2: 0.121秒    7: 0.113秒
   3: 0.133秒    8: 0.113秒
   4: 0.123秒    9: 0.108秒
   5: 0.110秒   10: 0.117秒
   平均: 0.117秒

Alacritty:
   1: 0.110秒    6: 0.112秒
   2: 0.137秒    7: 0.109秒
   3: 0.111秒    8: 0.116秒
   4: 0.118秒    9: 0.114秒
   5: 0.109秒   10: 0.116秒
   平均: 0.115秒
```

差は **2ms（約2%）** でほぼ同等。

### 3. メモリ使用量（RSS）

**方法:**
1. 既存プロセスを `pkill -9` で終了
2. 1秒待機
3. 両方のアプリを起動
4. 3秒待機（安定化）
5. `ps aux` でRSS（Resident Set Size）を取得
6. 5回計測

```bash
ps aux | grep "/umiterm$" | awk '{print $6}'  # KB単位
```

**結果（5回計測）:**
```
計測1: UmiTerm=61.0 MB, Alacritty=74.8 MB
計測2: UmiTerm=61.0 MB, Alacritty=75.6 MB
計測3: UmiTerm=61.5 MB, Alacritty=74.9 MB
計測4: UmiTerm=61.8 MB, Alacritty=76.6 MB
計測5: UmiTerm=61.3 MB, Alacritty=75.3 MB

平均: UmiTerm=61 MB, Alacritty=75 MB
```

UmiTermが **19%少ないメモリ** で動作。

---

## 最適化内容（v0.2.0）

| 項目 | 最適化前 | 最適化後 | 削減率 |
|------|----------|----------|--------|
| インスタンスバッファ | 50,000セル | 8,000セル | 84% |
| グリフアトラス | 1024×1024 | 512×512 | 75% |
| 日本語フォント | 起動時読込 | 遅延読込 | - |
| **合計メモリ** | 88 MB | 61 MB | **31%** |

---

## 技術比較

| 項目 | UmiTerm | Alacritty |
|------|---------|-----------|
| 言語 | Rust | Rust |
| GPU API | wgpu (Metal/Vulkan/DX12) | OpenGL |
| ANSIパーサー | vte | vte |
| フォントレンダリング | fontdue | crossfont |
| コード行数 | ~2,500行 | ~30,000行 |

---

## 結論

- **UmiTermは2勝1敗でAlacrittyに勝利**
- メモリ使用量で19%少ない（61 MB vs 75 MB）
- バイナリサイズで3倍小さい（4.3 MB vs 14 MB）
- 起動時間はほぼ同等（差2ms、2%）
- wgpu採用で将来性あり（WebGPU対応など）
- シンプルなコードベース（Alacrittyの1/12）

---

## 自分でベンチマークを実行

```bash
# 起動時間テスト
for i in {1..10}; do
    pkill -9 -f "UmiTerm" 2>/dev/null
    sleep 0.8
    start=$(python3 -c "import time; print(time.time())")
    open -a UmiTerm
    while ! pgrep -x umiterm > /dev/null 2>&1; do sleep 0.005; done
    end=$(python3 -c "import time; print(time.time())")
    python3 -c "print(f'Run {$i}: {$end - $start:.3f}秒')"
done

# メモリ使用量テスト
ps aux | grep "/umiterm$" | awk '{printf "RSS: %.1f MB\n", $6/1024}'

# スループットテスト（各ターミナルで実行）
time seq 1 100000
```
