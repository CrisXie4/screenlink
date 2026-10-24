//! 本地 IPC 快速通道（GUI <-> screenlink-mcp）。
//!
//! ## 设计
//! - TCP 127.0.0.1:27190，newline-delimited JSON-RPC（与 MCP 同风格，非 MCP 协议本身）。
//! - 鉴权：首条请求必须是 `hello {token}`，token 与 ai_access.json 校验一致才放行。
//! - 性能关键路径：
//!   * `screenshot`：GUI 存活的复用投屏最新帧（缓存 <1.5s 直接命中；否则向
//!     webview 请求 canvas 抓帧），无帧时才回退 adb screencap。目标 <60ms。
//!   * `tap/swipe/type/key`：经 GUI 已建立的 Scrcpy 控制 socket 注入，目标 <20ms。
//! - 二进制（截图 JPEG）以 base64 内嵌 JSON —— 本地回环带宽充裕，换来协议简单。
//! - GUI 未运行时，MCP 直接走 ADB 慢路径（见 screenlink-mcp/tools.rs）。

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub const IPC_PORT: u16 = 27190;

pub type Handler = Arc<
    dyn Fn(String, Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, (i32, String)>> + Send>>
        + Send
        + Sync,
>;

/// 启动 IPC server（GUI 侧）。端口被占用时返回 Ok(None)（说明已有实例在跑）。
pub async fn serve(handler: Handler) -> Result<()> {
    let listener = match TcpListener::bind(("127.0.0.1", IPC_PORT)).await {
        Ok(l) => l,
        Err(_) => return Ok(()), // 已有 GUI 实例持有端口
    };
    tracing::info!("IPC server listening on 127.0.0.1:{IPC_PORT}");
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let handler = handler.clone();
        tokio::spawn(async move {
            let _ = serve_conn(stream, handler).await;
        });
    }
}

async fn serve_conn(stream: TcpStream, handler: Handler) -> Result<()> {
    stream.set_nodelay(true).ok();
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    let mut authed = false;
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));

        if !authed {
            if method == "hello" {
                let token = params.get("token").and_then(|t| t.as_str()).unwrap_or("");
                if crate::auth::AiAccess::verify(token) {
                    authed = true;
                    write_resp(&mut wr, id, json!({"ok": true, "server": "screenlink-gui", "version": env!("CARGO_PKG_VERSION")}), None).await;
                } else {
                    write_resp(&mut wr, id, Value::Null, Some((-32001, "token 无效或 AI 访问已被禁用".into()))).await;
                    break;
                }
            } else {
                write_resp(&mut wr, id, Value::Null, Some((-32002, "未鉴权".into()))).await;
                break;
            }
            continue;
        }

        let resp = handler(method, params).await;
        match resp {
            Ok(v) => write_resp(&mut wr, id, v, None).await,
            Err((code, msg)) => write_resp(&mut wr, id, Value::Null, Some((code, msg))).await,
        }
    }
    Ok(())
}

async fn write_resp(
    wr: &mut tokio::net::tcp::OwnedWriteHalf,
    id: Value,
    result: Value,
    error: Option<(i32, String)>,
) {
    let body = json!({
        "id": id,
        "result": result,
        "error": error.map(|(code, message)| json!({"code": code, "message": message})),
    });
    let mut line = body.to_string();
    line.push('\n');
    let _ = wr.write_all(line.as_bytes()).await;
    let _ = wr.flush().await;
}

// ---- 页面结构解析（uiautomator dump XML -> 结构化元素，供 screenlink_page_state） ----

pub fn page_elements_from_xml(xml: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return out;
    };
    let mut idx = 0usize;
    for node in doc.descendants() {
        if node.tag_name().name() != "node" {
            continue;
        }
        let a = |k: &str| node.attribute(k).unwrap_or("");
        let bounds = a("bounds");
        let Some((cx, cy, x1, y1, x2, y2)) = center_of(bounds) else {
            continue;
        };
        let text = a("text");
        let desc = a("content-desc");
        let clickable = a("clickable") == "true";
        if !clickable && text.is_empty() && desc.is_empty() {
            continue;
        }
        out.push(json!({
            "index": idx,
            "class": a("class"),
            "text": text,
            "content_desc": desc,
            "resource_id": a("resource-id"),
            "clickable": clickable,
            "bounds": { "x1": x1, "y1": y1, "x2": x2, "y2": y2 },
            "center": [cx, cy],
        }));
        idx += 1;
    }
    out
}

fn center_of(bounds: &str) -> Option<(i32, i32, i32, i32, i32, i32)> {
    // "[x1,y1][x2,y2]"
    let nums: Vec<i32> = bounds
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() == 4 {
        Some((
            (nums[0] + nums[2]) / 2,
            (nums[1] + nums[3]) / 2,
            nums[0],
            nums[1],
            nums[2],
            nums[3],
        ))
    } else {
        None
    }
}

// ---- MCP 侧客户端 ----

#[derive(Clone)]
pub struct IpcClient {
    token: String,
}

impl IpcClient {
    pub fn new(token: String) -> Self {
        Self { token }
    }

    pub fn has_token(&self) -> bool {
        !self.token.is_empty()
    }

    /// 发起一次请求（每请求独立连接：本地回环成本可忽略，换来无状态可靠）。
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let req = json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "method": method,
            "params": params,
        });
        let mut payload = req.to_string();
        payload.push('\n');
        let s = tokio::time::timeout(
            std::time::Duration::from_millis(600),
            TcpStream::connect(("127.0.0.1", IPC_PORT)),
        )
        .await
        .map_err(|_| anyhow!("GUI IPC 不可达"))??;
        s.set_nodelay(true).ok();
        let (mut rd, mut wr) = s.into_split();
        wr.write_all(payload.as_bytes()).await?;
        wr.flush().await?;
        let mut reader = BufReader::new(rd);
        let mut line = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(30), reader.read_line(&mut line))
            .await
            .map_err(|_| anyhow!("IPC 响应超时"))??;
        let line = line.trim().to_string();
        if line.is_empty() {
            return Err(anyhow!("IPC 连接关闭"));
        }
        let resp: Value = serde_json::from_str(&line)?;
        if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("IPC 错误");
            return Err(anyhow!("{msg}"));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }
}
