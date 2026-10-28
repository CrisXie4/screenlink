//! PC ↔ 手机伴侣 App 协议客户端（PROTOCOL.md v1.1 最小集：配对 / 认证）。
//!
//! 帧格式：`SLNK | ver=1 | type | flags u16=0 | len u32 | payload`（全大端）。
//! 配对流程（码由手机端生成并显示，必须与 NEED_PAIR 同一连接提交）：
//! `HELLO(pc) → AUTH{格式合法的占位 token} → NEED_PAIR → (用户在 UI 输码) →
//! PAIR{code} → PAIR_OK{token, token_id, name}`。
//! 已配对设备：`HELLO → AUTH{真 token} → AUTH_OK`。
//!
//! 保活：手机端 75s 无入帧断连；配对等待输入期间由调用方周期发 PING 续命。

use crate::util::data_dir;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

pub const T_HELLO: u8 = 0x01;
pub const T_AUTH: u8 = 0x02;
pub const T_AUTH_OK: u8 = 0x03;
pub const T_NEED_PAIR: u8 = 0x04;
pub const T_PAIR: u8 = 0x05;
pub const T_PAIR_OK: u8 = 0x06;
pub const T_PAIR_FAIL: u8 = 0x07;
pub const T_PING: u8 = 0x30;
pub const T_PONG: u8 = 0x31;
pub const T_ERR: u8 = 0x40;
pub const T_BYE: u8 = 0x41;

const MAGIC: &[u8; 4] = b"SLNK";
const DEFAULT_PORT: u16 = 27188;

/// 已配对手机（PC 侧持久化；token 为明文——PC 是信任端，与手机端只存哈希对偶）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedPhone {
    pub token_id: String,
    pub token: String,
    pub name: String,
    pub ip: String,
    pub paired_at: u64,
}

fn store_path() -> std::path::PathBuf {
    data_dir().join("phone-paired.json")
}

pub fn load_paired() -> Vec<PairedPhone> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn upsert_paired(phone: PairedPhone) {
    let mut list = load_paired();
    list.retain(|p| p.token_id != phone.token_id);
    list.push(phone);
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(s) = serde_json::to_string_pretty(&list) {
        let _ = std::fs::write(store_path(), s);
    }
}

pub fn remove_paired(token_id: &str) {
    let mut list = load_paired();
    list.retain(|p| p.token_id != token_id);
    let _ = std::fs::write(store_path(), serde_json::to_string(&list).unwrap_or_default());
}

/// 配对会话：begin 后由 UI 展示"输入手机上显示的配对码"，submit 在同连接上提交。
pub struct PairingSession {
    reader: Mutex<tokio::net::tcp::OwnedReadHalf>,
    writer: std::sync::Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    stop: tokio::sync::watch::Sender<bool>,
    pub ip: String,
}

async fn write_frame(w: &mut (impl AsyncWriteExt + Unpin), t: u8, payload: &[u8]) -> Result<()> {
    let mut head = Vec::with_capacity(12 + payload.len());
    head.extend_from_slice(MAGIC);
    head.push(1);
    head.push(t);
    head.extend_from_slice(&0u16.to_be_bytes());
    head.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    head.extend_from_slice(payload);
    w.write_all(&head).await?;
    w.flush().await?;
    Ok(())
}

async fn read_frame(r: &mut (impl AsyncReadExt + Unpin)) -> Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 12];
    r.read_exact(&mut head).await?;
    if &head[0..4] != MAGIC {
        return Err(anyhow!("协议头不匹配"));
    }
    let t = head[5];
    let len = u32::from_be_bytes([head[8], head[9], head[10], head[11]]) as usize;
    if len > 4 * 1024 * 1024 {
        return Err(anyhow!("payload 超限"));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut payload).await?;
    }
    Ok((t, payload))
}

async fn read_json(r: &mut (impl AsyncReadExt + Unpin)) -> Result<(u8, serde_json::Value)> {
    let (t, p) = read_frame(r).await?;
    let obj = if p.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&p).unwrap_or(serde_json::Value::Null)
    };
    Ok((t, obj))
}

fn jstr(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}

fn split_stream(stream: TcpStream) -> (tokio::net::tcp::OwnedReadHalf, tokio::net::tcp::OwnedWriteHalf) {
    stream.into_split()
}

/// 建立配对会话：连接 → 双方 HELLO → AUTH（占位 token，触发 NEED_PAIR 与手机端出码）。
pub async fn pair_begin(ip: &str, port: u16) -> Result<PairingSession> {
    let addr = if ip.contains(':') { ip.to_string() } else { format!("{ip}:{DEFAULT_PORT}") };
    let stream = TcpStream::connect(&addr).await.context("连接手机失败（同网段？App 服务在运行？）")?;
    stream.set_nodelay(true)?;
    let (mut r, mut w) = split_stream(stream);

    write_frame(
        &mut w,
        T_HELLO,
        format!(
            "{{\"role\":\"pc\",\"name\":\"{}\",\"app\":\"screenlink-pc/0.1.0\",\"pv\":1}}",
            std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
        ).as_bytes(),
    )
    .await?;
    let (t, obj) = read_json(&mut r).await?;
    if t != T_HELLO {
        return Err(anyhow!("手机未返回 HELLO（type {t:#x}）"));
    }
    let phone_name = jstr(&obj, "name");

    // 占位 token：格式合法（43 字符 base64url + 8 hex）但必然未配对 → NEED_PAIR
    write_frame(
        &mut w,
        T_AUTH,
        br#"{"token":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","token_id":"deadbeef"}"#,
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let phone_name = loop {
        let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        if timeout.is_zero() {
            return Err(anyhow!("等待配对应答超时"));
        }
        let (t, obj) = tokio::time::timeout(timeout, read_json(&mut r)).await??;
        match t {
            T_NEED_PAIR => break phone_name,
            T_PING => write_frame(&mut w, T_PONG, b"{}").await?,
            _ => return Err(anyhow!("意外的应答（type {t:#x}）：{}", obj)),
        }
    };
    let _ = phone_name;

    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let keep_writer = std::sync::Arc::new(Mutex::new(w));
    let kw = keep_writer.clone();
    // 保活：每 25s 一个 PING 入帧，防 75s 空闲断连（submit 前用户输入期间）
    tokio::spawn(async move {
        let mut w = kw;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(25)) => {
                    let mut guard = w.lock().await;
                    if write_frame(&mut *guard, T_PING, b"{}").await.is_err() { return; }
                }
                _ = stop_rx.changed() => return,
            }
        }
    });

    Ok(PairingSession {
        reader: Mutex::new(r),
        writer: keep_writer,
        stop: stop_tx,
        ip: addr,
    })
}

impl PairingSession {
    /// 提交配对码：PAIR{code} → PAIR_OK（保存 token）| PAIR_FAIL（剩余次数）。
    pub async fn submit_code(&self, code: &str) -> Result<PairedPhone> {
        let mut w = self.writer.lock().await;
        write_frame(&mut *w, T_PAIR, format!("{{\"code\":\"{code}\"}}").as_bytes()).await?;
        drop(w);
        let mut r = self.reader.lock().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            if timeout.is_zero() {
                return Err(anyhow!("等待配对结果超时"));
            }
            let (t, obj) = tokio::time::timeout(timeout, read_json(&mut *r)).await??;
            match t {
                T_PAIR_OK => {
                    let phone = PairedPhone {
                        token: jstr(&obj, "token"),
                        token_id: jstr(&obj, "token_id"),
                        name: jstr(&obj, "name"),
                        ip: self.ip.clone(),
                        paired_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                    };
                    if phone.token.len() != 43 || phone.token_id.len() != 8 {
                        return Err(anyhow!("手机返回的 token 格式异常"));
                    }
                    upsert_paired(phone.clone());
                    return Ok(phone);
                }
                T_PAIR_FAIL => {
                    let left = obj.get("attempts_left").and_then(|v| v.as_i64()).unwrap_or(0);
                    return Err(anyhow!("配对码不正确（剩余尝试 {left} 次；为 0 请重新开始配对）"));
                }
                T_PING => {
                    let mut w = self.writer.lock().await;
                    write_frame(&mut *w, T_PONG, b"{}").await?;
                }
                _ => return Err(anyhow!("配对流程被中断（type {t:#x}）")),
            }
        }
    }

    pub fn cancel(&self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for PairingSession {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// 已配对设备连接校验：HELLO → AUTH{真 token} → AUTH_OK → BYE。
pub async fn verify(ip: &str, token: &str) -> Result<String> {
    let token_id = sha256_hex(token.as_bytes())[..8].to_string();
    let addr = if ip.contains(':') { ip.to_string() } else { format!("{ip}:{DEFAULT_PORT}") };
    let mut stream = TcpStream::connect(&addr).await.context("连接手机失败")?;
    stream.set_nodelay(true)?;
    write_frame(
        &mut stream,
        T_HELLO,
        format!(
            "{{\"role\":\"pc\",\"name\":\"{}\",\"app\":\"screenlink-pc/0.1.0\",\"pv\":1}}",
            std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
        ).as_bytes(),
    )
    .await?;
    let (t, _) = read_json(&mut stream).await?;
    if t != T_HELLO {
        return Err(anyhow!("手机未返回 HELLO"));
    }
    write_frame(
        &mut stream,
        T_AUTH,
        format!("{{\"token\":\"{token}\",\"token_id\":\"{token_id}\"}}").as_bytes(),
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        if timeout.is_zero() {
            return Err(anyhow!("认证应答超时"));
        }
        let (t, obj) = tokio::time::timeout(timeout, read_json(&mut stream)).await??;
        match t {
            T_AUTH_OK => {
                let name = jstr(&obj, "name");
                let _ = write_frame(&mut stream, T_BYE, b"{\"reason\":\"done\"}").await;
                return Ok(name);
            }
            T_PING => write_frame(&mut stream, T_PONG, b"{}").await?,
            T_ERR => return Err(anyhow!("手机拒绝：{}", obj)),
            _ => return Err(anyhow!("认证失败（type {t:#x}）——token 可能已失效，请重新配对")),
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    // 轻量 SHA-256（仅用于 token_id 派生，与手机端 Digests.sha256Hex 对齐）
    use std::fmt::Write;
    let digest = sha256(data);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 最小 SHA-256 实现（无外部依赖；输入为 32B token，性能无要求）。
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g; g = f; f = e; e = d.wrapping_add(t1); d = c; c = b; b = a; a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b); h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e); h[5] = h[5].wrapping_add(f); h[6] = h[6].wrapping_add(g); h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}
