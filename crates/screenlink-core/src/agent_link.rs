//! PC ↔ 手机 agent 数据面（PROTOCOL.md v1.1 §5/§6）：
//! 长连接 + SEND 推送 + offset 断点续传 + 滚动 MD5 校验。
//!
//! 与 agent_client.rs 的关系：那边是控制面（配对/校验，用完即断），
//! 这里是数据面（单连接多路复用，一次连接传完整个任务的所有文件）。
//!
//! 用法：`AgentLink::connect()` → 逐文件 `send_file()` → drop（自动 BYE）。
//! 任何一步失败由调用方决定回退 adb（本层不重试，避免与上层冲突策略纠缠）。

use crate::agent_client::{
    read_frame, write_frame, T_BYE, T_CANCEL, T_DATA, T_ERR, T_PING, T_PONG, T_PROGRESS,
    T_SEND_ACK, T_SEND_REQ, T_TRANSFER_DONE,
};
use crate::adb::CancelToken;
use anyhow::{anyhow, Context, Result};
use md5::{Digest, Md5};
use std::io::Seek;
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

pub const T_HELLO: u8 = 0x01;
pub const T_AUTH: u8 = 0x02;
pub const T_AUTH_OK: u8 = 0x03;
pub const T_NEED_PAIR: u8 = 0x04;

/// DATA 帧单帧数据上限（协议 §5.5）
const DATA_CHUNK: usize = 1024 * 1024;
/// 连接超时：局域网直连应当毫秒级，超时立即回退 adb
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);
/// 单帧应答超时（SEND_ACK / TRANSFER_DONE）
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// agent 通道目标（由 GUI 侧按 beacon + 配对记录解析）
#[derive(Debug, Clone)]
pub struct AgentTarget {
    /// "ip:port"
    pub addr: String,
    pub token: String,
}

pub struct AgentLink {
    rd: tokio::net::tcp::OwnedReadHalf,
    wr: tokio::net::tcp::OwnedWriteHalf,
    next_xid: u32,
}

impl AgentLink {
    /// 连接并完成 HELLO/AUTH 握手（超时从紧：失败快速回退 adb）。
    pub async fn connect(target: &AgentTarget) -> Result<Self> {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&target.addr))
            .await
            .map_err(|_| anyhow!("连接超时（{target_addr}）", target_addr = target.addr))??;
        stream.set_nodelay(true)?;
        let (mut rd, mut wr) = stream.into_split();
        let hello = format!(
            "{{\"role\":\"pc\",\"name\":\"{}\",\"app\":\"screenlink-pc/0.1.0\",\"pv\":1}}",
            std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
        );
        write_frame(&mut wr, T_HELLO, hello.as_bytes()).await?;
        let (t, _) = read_json_timeout(&mut rd).await?;
        if t != T_HELLO {
            return Err(anyhow!("手机未返回 HELLO（type {t:#x}）"));
        }
        let token_id = crate::agent_client::token_id_of(&target.token);
        let auth = format!("{{\"token\":\"{}\",\"token_id\":\"{token_id}\"}}", target.token);
        write_frame(&mut wr, T_AUTH, auth.as_bytes()).await?;
        let (t, obj) = read_json_timeout(&mut rd).await?;
        match t {
            T_AUTH_OK => {}
            T_NEED_PAIR => return Err(anyhow!("token 未配对（实例不匹配？）")),
            T_ERR => return Err(anyhow!("手机拒绝：{obj}")),
            _ => return Err(anyhow!("认证失败（type {t:#x}）")),
        }
        Ok(Self { rd, wr, next_xid: 0x1000 })
    }

    /// 推送一个文件（含 offset 续传协商 + 滚动 MD5 终验）。
    /// `on_progress(copied, total)` 在每次写出 DATA 后回调（节流由调用方处理）。
    pub async fn send_file(
        &mut self,
        local: &Path,
        name: &str,
        cancel: &CancelToken,
        mut on_progress: impl FnMut(u64, u64),
    ) -> Result<()> {
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(anyhow!("文件名含路径分隔符（协议 §5.3）：{name}"));
        }
        let size = std::fs::metadata(local).map(|m| m.len()).context("读文件大小失败")?;
        let xid = self.next_xid.max(1);
        self.next_xid = self.next_xid.wrapping_add(1).max(1);

        // SEND_REQ：md5 置空（发送中滚动计算，收满后与 TRANSFER_DONE.md5 终验）
        let req = format!(
            "{{\"xid\":{xid},\"name\":{},\"size\":{size},\"mime\":\"application/octet-stream\",\"md5\":null}}",
            json_str(name),
        );
        write_frame(&mut self.wr, T_SEND_REQ, req.as_bytes()).await?;
        let (t, obj) = read_json_timeout(&mut self.rd).await?;
        if t != T_SEND_ACK {
            return Err(anyhow!("期望 SEND_ACK 得到 type {t:#x}: {obj}"));
        }
        let ack_xid = obj.get("xid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if ack_xid != xid {
            return Err(anyhow!("SEND_ACK xid 不匹配（{ack_xid} ≠ {xid}）"));
        }
        if obj.get("accept").and_then(|v| v.as_bool()) != Some(true) {
            let reason = obj.get("reason").and_then(|v| v.as_str()).unwrap_or("UNKNOWN");
            return Err(anyhow!("手机拒绝接收：{reason}"));
        }
        let offset = obj.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
        if offset > size {
            return Err(anyhow!("手机端 offset 越界（{offset} > {size}）"));
        }

        // 滚动 MD5：先补算已续传前缀，再随发送流式累计
        let mut file = std::fs::File::open(local).context("打开本地文件失败")?;
        let mut hasher = Md5::new();
        if offset > 0 {
            use std::io::Read;
            let prefix_len = offset;
            std::io::copy(
                &mut std::io::Read::by_ref(&mut file).take(prefix_len),
                &mut HashWriter(&mut hasher),
            )
            .context("前缀 re-hash 失败")?;
        }
        file.seek(std::io::SeekFrom::Start(offset))?;

        let mut buf = vec![0u8; DATA_CHUNK];
        let mut sent: u64 = offset;
        let mut reader = tokio::fs::File::from(file);
        loop {
            if cancel.is_cancelled() {
                let _ = write_frame(
                    &mut self.wr,
                    T_CANCEL,
                    format!("{{\"xid\":{xid},\"keep\":true,\"reason\":\"user\"}}").as_bytes(),
                )
                .await;
                return Err(anyhow!("已取消"));
            }
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let mut frame = Vec::with_capacity(4 + n);
            frame.extend_from_slice(&xid.to_be_bytes());
            frame.extend_from_slice(&buf[..n]);
            write_frame(&mut self.wr, T_DATA, &frame).await?;
            hasher.update(&buf[..n]);
            sent += n as u64;
            on_progress(sent, size);
        }
        if sent != size {
            return Err(anyhow!("发送不完整（{sent}/{size}）"));
        }

        // 等 TRANSFER_DONE（跳过 PROGRESS，代答 PING）
        let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
        loop {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            if timeout.is_zero() {
                return Err(anyhow!("等待 TRANSFER_DONE 超时"));
            }
            let (t, payload) =
                tokio::time::timeout(timeout, read_frame(&mut self.rd)).await??;
            match t {
                T_TRANSFER_DONE => {
                    let obj: serde_json::Value = serde_json::from_slice(&payload)?;
                    let done_xid = obj.get("xid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    if done_xid != xid {
                        continue; // 其它传输的尾帧（理论上不会出现，防御）
                    }
                    let status = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    if status != "OK" {
                        return Err(anyhow!("手机端结算异常：{status}"));
                    }
                    let remote_md5 = obj.get("md5").and_then(|v| v.as_str()).unwrap_or("");
                    let digest = hasher.finalize();
                    let local_md5 = hex(&digest);
                    if !remote_md5.is_empty() && remote_md5 != local_md5 {
                        return Err(anyhow!("MD5 不一致（本 {local_md5} / 远 {remote_md5}）"));
                    }
                    return Ok(());
                }
                T_PROGRESS => continue,
                T_PING => write_frame(&mut self.wr, T_PONG, b"{}").await?,
                T_ERR => return Err(anyhow!("手机错误：{}", String::from_utf8_lossy(&payload))),
                _ => continue, // 未知帧忽略（向前兼容）
            }
        }
    }
}

impl Drop for AgentLink {
    fn drop(&mut self) {
        // Drop 在同步上下文，无法 await：BYE 小帧（29B）用 try_write 尽力而为，
        // 失败由对端 75s 空闲超时兜底
        let mut frame = Vec::with_capacity(12 + 17);
        frame.extend_from_slice(b"SLNK");
        frame.push(1);
        frame.push(T_BYE);
        frame.extend_from_slice(&0u16.to_be_bytes());
        let payload = b"{\"reason\":\"done\"}";
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        let _ = self.wr.try_write(&frame);
    }
}

async fn read_json_timeout(
    r: &mut (impl AsyncReadExt + Unpin),
) -> Result<(u8, serde_json::Value)> {
    let (t, payload) = tokio::time::timeout(REPLY_TIMEOUT, read_frame(r)).await??;
    let obj = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(serde_json::Value::Null)
    };
    Ok((t, obj))
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 把写入的字节同时喂给 MD5 的薄包装（配合 std::io::copy 做前缀 re-hash）
struct HashWriter<'a>(&'a mut Md5);

impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_str_escapes() {
        assert_eq!(json_str("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn hex_lower() {
        assert_eq!(hex(&[0xde, 0xad]), "dead");
    }
}
