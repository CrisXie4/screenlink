//! 局域网设备发现（专项设计）：
//!
//! 1. **mDNS（主路径）**：安卓 11+「无线调试」会通过 mDNS 广播
//!    `_adb-tls-connect._tcp.`（可直连）与 `_adb-tls-pairing._tcp.`（待配对）。
//!    直接解析 `adb mdns services` 的输出，无需额外 mDNS 栈，天然兼容 adb 鉴权。
//! 2. **UDP 广播（辅助）**：PC 广播 SCREENLINK_DISCOVER，装有 ScreenLink agent
//!    的设备回 JSON（name/ip/port/serial）——为自建 TCP 高速通道预留的发现路径。
//! 3. **子网扫描（兜底）**：对同网段 5555 端口（经典 adb tcpip）做并发 TCP 探测。
//! 4. 发现失败时 UI 支持手动输入 IP:端口 + 配对码（无线调试）。

use crate::adb::Adb;
use crate::device::{MdnsKind, MdnsService};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::time::Duration;

pub const UDP_DISCOVER_PORT: u16 = 27186;
const UDP_MAGIC: &[u8] = b"SCREENLINK_DISCOVER_V1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpAgentReply {
    pub name: String,
    pub serial: String,
    pub ip: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryResult {
    pub mdns: Vec<MdnsService>,
    pub udp: Vec<UdpAgentReply>,
    pub scanned_5555: Vec<String>,
}

pub struct Discovery;

impl Discovery {
    /// 一次完整发现：mDNS + UDP 广播 + 5555 扫描（后两者可失败，不影响主路径）。
    pub async fn scan(adb: &Adb) -> DiscoveryResult {
        let mdns = adb.mdns_services().await.unwrap_or_default();
        let udp = Self::udp_broadcast().await.unwrap_or_default();
        let scanned_5555 = Self::scan_subnet_5555().await.unwrap_or_default();
        DiscoveryResult {
            mdns,
            udp,
            scanned_5555,
        }
    }

    /// UDP 广播发现（设备端 ScreenLink agent 响应）。
    pub async fn udp_broadcast() -> Result<Vec<UdpAgentReply>> {
        use tokio::net::UdpSocket;
        let sock = UdpSocket::bind(("0.0.0.0", 0)).await?;
        sock.set_broadcast(true)?;
        // 发往受限广播地址与各网段定向广播，提高命中率
        let mut targets = vec![(Ipv4Addr::BROADCAST, UDP_DISCOVER_PORT)];
        for ip in Self::local_ips() {
            let net = u32::from(ip) & 0xFFFF_FF00;
            targets.push((Ipv4Addr::from(net | 0xFF), UDP_DISCOVER_PORT));
        }
        for (addr, port) in targets {
            let _ = sock
                .send_to(UDP_MAGIC, std::net::SocketAddr::from((addr, port)))
                .await;
        }
        let mut replies = Vec::new();
        let mut buf = [0u8; 2048];
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1200);
        loop {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            if timeout.is_zero() {
                break;
            }
            match tokio::time::timeout(timeout, sock.recv_from(&mut buf)).await {
                Ok(Ok((n, src))) => {
                    if let Ok(txt) = std::str::from_utf8(&buf[..n]) {
                        if let Ok(mut r) = serde_json::from_str::<UdpAgentReply>(txt) {
                            if r.ip.is_empty() {
                                r.ip = src.ip().to_string();
                            }
                            // 应答来源在虚拟/保留段的一律丢弃（防 TUN/代理伪造源）
                            if let Ok(src_ip) = src.ip().to_string().parse::<Ipv4Addr>() {
                                if crate::util::is_virtual_or_reserved_ipv4(src_ip) {
                                    continue;
                                }
                            }
                            if !replies.iter().any(|x: &UdpAgentReply| x.serial == r.serial) {
                                replies.push(r);
                            }
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(replies)
    }

    /// 同网段 5555 端口并发探测（经典 `adb tcpip 5555` 设备）。
    pub async fn scan_subnet_5555() -> Result<Vec<String>> {
        let mut found = Vec::new();
        // 私有网段优先（多网卡时虚拟网卡虽已被 local_ips 过滤，这里再保一层顺序）
        let mut ips = Self::local_ips();
        ips.sort_by_key(|ip| !ip.is_private());
        for ip in ips {
            let net = u32::from(ip) & 0xFFFF_FF00;
            let base = Ipv4Addr::from(net);
            let octets = base.octets();
            if octets[0] == 127 {
                continue;
            }
            let mut handles = Vec::new();
            for host in 1..255u8 {
                let target = Ipv4Addr::new(octets[0], octets[1], octets[2], host);
                handles.push(tokio::spawn(async move {
                    let addr = std::net::SocketAddr::from((target, 5555));
                    if tokio::time::timeout(Duration::from_millis(250), tokio::net::TcpStream::connect(addr))
                        .await
                        .is_ok()
                    {
                        Some(target.to_string())
                    } else {
                        None
                    }
                }));
            }
            for h in handles {
                if let Ok(Some(ip)) = h.await {
                    found.push(format!("{ip}:5555"));
                }
            }
            break; // 默认只扫主网段，避免多网卡时全量扫
        }
        Ok(found)
    }

    pub fn local_ips() -> Vec<Ipv4Addr> {
        if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|i| match i.addr {
                if_addrs::IfAddr::V4(v4) => Some(v4.ip),
                _ => None,
            })
            // 排除回环与虚拟网卡/保留段（Clash fake-ip TUN、链路本地、CGNAT 等）
            .filter(|ip| !crate::util::is_virtual_or_reserved_ipv4(*ip))
            .collect()
    }
}

/// mDNS 结果分类标注（供 UI 展示"可连接 / 待配对"）。
pub fn classify(svc: &MdnsService) -> &'static str {
    match svc.kind {
        MdnsKind::Connect => "已配对可连接",
        MdnsKind::Pairing => "待配对",
    }
}
