//! 平台工具：进程启动、路径、编解码小工具。

use std::path::PathBuf;
use tokio::io::AsyncReadExt;

/// Windows 下隐藏 adb 子进程的控制台窗口。
pub fn no_window(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd;
    cmd
}

/// ScreenLink 数据目录：Windows -> %APPDATA%\screenlink，其它平台 -> ~/.screenlink
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let d = PathBuf::from(appdata).join("screenlink");
            let _ = std::fs::create_dir_all(&d);
            return d;
        }
    }
    let d = dirs_home().join(".screenlink");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn dirs_home() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// 精确读取 n 字节（用于 Scrcpy 线协议，二进制布局见 scrcpy/protocol.rs）。
pub async fn read_exact<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
    n: usize,
) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub fn parse_u32_be(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub fn parse_i64_be(b: &[u8]) -> i64 {
    i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// 人类可读速度（B/s -> "12.3 MB/s"）
pub fn fmt_speed(bps: f64) -> String {
    const U: [&str; 5] = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"];
    let mut v = bps;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, U[i])
}

pub fn fmt_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, U[i])
}
