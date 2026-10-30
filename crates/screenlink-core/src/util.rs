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

/// 虚拟网卡 / 保留网段判定：这类地址不得参与局域网扫描与设备发现。
///
/// 关键是 198.18.0.0/15（RFC 2544 基准测试保留段）：Clash/Mihomo 等代理的
/// fake-ip 默认段，TUN 虚拟网卡会把发往该段任意端口的 TCP 连接全部"接通"，
/// 子网扫描会得到整段假设备（表现为扫描页刷出 254 个 198.18.0.x:5555）。
pub fn is_virtual_or_reserved_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
        return true; // RFC 2544 / fake-ip
    }
    if o[0] == 169 && o[1] == 254 {
        return true; // 链路本地（虚拟网卡常见的未连接地址）
    }
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return true; // CGNAT（Tailscale 等虚拟组网）
    }
    o[0] >= 224 // 组播 + 保留段
}

/// 解析 "ip:port" 形式 serial 中的 IPv4 并判定是否虚拟/保留段。
pub fn serial_in_virtual_range(serial: &str) -> bool {
    let Some((ip_part, _)) = serial.split_once(':') else {
        return false;
    };
    ip_part
        .parse::<std::net::Ipv4Addr>()
        .map(is_virtual_or_reserved_ipv4)
        .unwrap_or(false)
}
