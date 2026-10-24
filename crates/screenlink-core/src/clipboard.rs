//! 剪贴板双向同步。
//!
//! 通道：PC <-> 手机走 Scrcpy 控制协议（SET_CLIPBOARD / DEVICE_CLIPBOARD 事件），
//! PC 本机剪贴板经 arboard 跨平台读写。轮询检测本机变化（400ms），
//! 代价可忽略；双向去重靠 last_hash 环。

use arboard::Clipboard;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct ClipboardSync {
    stop_tx: Arc<std::sync::mpsc::Sender<()>>,
    pub last_local_hash: Arc<AtomicU64>,
    pub last_remote_hash: Arc<AtomicU64>,
}

impl ClipboardSync {
    /// 启动本机剪贴板监听。本机变化 -> tx 发送文本（由上层转发 SET_CLIPBOARD 到手机）。
    pub fn start(tx: mpsc::UnboundedSender<String>) -> Self {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let last_local_hash = Arc::new(AtomicU64::new(0));
        let last_remote_hash = Arc::new(AtomicU64::new(0));
        let llh = last_local_hash.clone();
        let lrh = last_remote_hash.clone();
        std::thread::spawn(move || {
            let stop = stop_rx;
            let mut clip = match Clipboard::new() {
                Ok(c) => c,
                Err(_) => return,
            };
            let mut last_text = String::new();
            loop {
                if stop.try_recv().is_ok() {
                    break;
                }
                if let Ok(text) = clip.get_text() {
                    if !text.is_empty() && text != last_text && fnv1a(&text) != llh.load(Ordering::Relaxed) {
                        last_text = text.clone();
                        llh.store(fnv1a(&text), Ordering::Relaxed);
                        lrh.store(fnv1a(&text), Ordering::Relaxed); // 避免回环
                        if tx.send(text).is_err() {
                            break;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
        });
        Self {
            stop_tx: Arc::new(stop_tx),
            last_local_hash,
            last_remote_hash,
        }
    }

    /// 手机 -> PC：写入本机剪贴板（跳过与远端 hash 相同的内容防回环）。
    pub fn apply_remote(&self, text: &str) {
        let h = fnv1a(text);
        if h == self.last_remote_hash.load(Ordering::Relaxed) || h == self.last_local_hash.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut clip) = Clipboard::new() {
            if clip.set_text(text.to_string()).is_ok() {
                self.last_remote_hash.store(h, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for ClipboardSync {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
    }
}

pub fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
