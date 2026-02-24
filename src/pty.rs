//! PTY（擬似端末）モジュール
//!
//! シェルプロセスとの通信を担当
//! ノンブロッキングI/Oで高速に処理

use std::io::{Read, Write};
use std::sync::Arc;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

// ═══════════════════════════════════════════════════════════════════════════
// PTY マネージャー
// ═══════════════════════════════════════════════════════════════════════════

/// PTY（擬似端末）を管理する構造体
/// 別スレッドでI/Oを処理し、メインスレッドをブロックしない
pub struct Pty {
    /// マスターPTY（書き込み用）
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// シェルからの出力を受け取るレシーバー
    output_rx: Receiver<Vec<u8>>,
    /// シェルへの入力を送るセンダー
    input_tx: Sender<Vec<u8>>,
    /// 現在のサイズ
    size: PtySize,
}

impl Pty {
    /// 新しいPTYを作成し、シェルを起動
    ///
    /// # Arguments
    /// * `cols` - 列数
    /// * `rows` - 行数
    /// * `shell` - 起動するシェル（Noneでデフォルト）
    pub fn spawn(cols: u16, rows: u16, shell: Option<&str>) -> Result<Self> {
        // PTYシステムを取得
        let pty_system = native_pty_system();

        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        // PTYペアを作成
        let pair = pty_system
            .openpty(size)
            .context("PTYのオープンに失敗")?;

        // シェルコマンドを構築
        let shell_path = shell.map(String::from).unwrap_or_else(|| {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
        });

        let mut cmd = CommandBuilder::new(&shell_path);
        cmd.arg("-l"); // ログインシェルとして起動（.bash_profile等を読み込む）
        cmd.cwd(std::env::var("HOME").unwrap_or_else(|_| "/".into()));

        // 環境変数を設定
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        // 子プロセスを起動
        let _child = pair
            .slave
            .spawn_command(cmd)
            .context("シェルの起動に失敗")?;

        // マスターPTYのリーダーとライターを取得
        let master = pair.master;

        // チャネルを作成（バッファ付きで高速に）
        let (output_tx, output_rx) = bounded::<Vec<u8>>(256);
        let (input_tx, input_rx) = bounded::<Vec<u8>>(256);

        // 読み取りスレッドを起動
        let mut reader = master
            .try_clone_reader()
            .context("リーダーの複製に失敗")?;

        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buffer = [0u8; 8192]; // 大きめのバッファで読み取り回数を減らす

                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            // チャネルに送信（満杯なら古いデータを捨てる）
                            let _ = output_tx.try_send(buffer[..n].to_vec());
                        }
                        Err(e) => {
                            log::error!("PTY読み取りエラー: {}", e);
                            break;
                        }
                    }
                }
            })?;

        // 書き込み用のライターを取得
        let mut writer = master
            .take_writer()
            .context("ライターの取得に失敗")?;

        let master_arc = Arc::new(Mutex::new(master));

        // 書き込みスレッドを起動
        std::thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                while let Ok(data) = input_rx.recv() {
                    if let Err(e) = writer.write_all(&data) {
                        log::error!("PTY書き込みエラー: {}", e);
                        break;
                    }
                    let _ = writer.flush();
                }
            })?;

        Ok(Self {
            master: master_arc,
            output_rx,
            input_tx,
            size,
        })
    }

    /// シェルへデータを送信
    #[inline]
    pub fn write(&self, data: &[u8]) -> Result<()> {
        self.input_tx
            .send(data.to_vec())
            .context("入力チャネルへの送信に失敗")?;
        Ok(())
    }

    /// シェルからのデータを受信（ノンブロッキング）
    /// 利用可能なすべてのデータを返す
    #[inline]
    pub fn read(&self) -> Option<Vec<u8>> {
        // ノンブロッキングで受信を試みる
        let mut result = Vec::new();

        while let Ok(data) = self.output_rx.try_recv() {
            result.extend(data);
        }

        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    /// PTYのサイズを変更
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.size.cols = cols;
        self.size.rows = rows;

        let master = self.master.lock();
        master
            .resize(self.size)
            .context("PTYのリサイズに失敗")?;

        Ok(())
    }

    /// 現在のサイズを取得
    pub fn size(&self) -> (u16, u16) {
        (self.size.cols, self.size.rows)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// テスト
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_spawn() {
        // PTYが作成できることを確認
        let pty = Pty::spawn(80, 24, Some("/bin/echo")).unwrap();
        assert_eq!(pty.size(), (80, 24));
    }
}
