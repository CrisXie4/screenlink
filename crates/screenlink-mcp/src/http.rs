//! HTTP 传输（Streamable HTTP 简化实现）：远程 AI 客户端场景。
//! POST /mcp 接收 JSON-RPC，响应 application/json（无状态模式，符合 MCP 规范的
//! stateless HTTP 变体；SSE 流式响应为可选扩展，此处不依赖）。

use crate::jsonrpc;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub async fn serve(ctx: Arc<crate::ctx::Ctx>, port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("[screenlink-mcp] http transport on http://127.0.0.1:{port}/mcp");
    loop {
        let (stream, _) = listener.accept().await?;
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _ = handle_conn(stream, ctx).await;
        });
    }
}

async fn handle_conn(
    stream: tokio::net::TcpStream,
    ctx: Arc<crate::ctx::Ctx>,
) -> anyhow::Result<()> {
    stream.set_nodelay(true).ok();
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let request_line = read_line(&mut reader).await?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let mut content_length = 0usize;
    loop {
        let line = read_line(&mut reader).await?;
        if line.is_empty() {
            break;
        }
        let lower = line.to_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }

    if !(method == "POST" && (path == "/mcp" || path.starts_with("/mcp?"))) {
        let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        wr.write_all(resp.as_bytes()).await?;
        return Ok(());
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let body = format!("{{\"error\":\"bad json: {e}\"}}");
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            wr.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    };
    // 支持单条请求或 batch
    let responses: Vec<Value> = if msg.is_array() {
        let mut out = Vec::new();
        for m in msg.as_array().unwrap() {
            if let Some(r) = jsonrpc::handle(&ctx, m).await {
                out.push(r);
            }
        }
        out
    } else if let Some(r) = jsonrpc::handle(&ctx, &msg).await {
        vec![r]
    } else {
        Vec::new()
    };
    if responses.is_empty() {
        let resp = "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        wr.write_all(resp.as_bytes()).await?;
        return Ok(());
    }
    let payload = if responses.len() == 1 {
        responses[0].to_string()
    } else {
        Value::Array(responses).to_string()
    };
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    wr.write_all(resp.as_bytes()).await?;
    Ok(())
}

async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
    r: &mut R,
) -> anyhow::Result<String> {
    let mut line = String::new();
    let n = r.read_line(&mut line).await?;
    if n == 0 {
        anyhow::bail!("eof");
    }
    Ok(line.trim_end().to_string())
}
