//! stdio 传输：Claude Desktop / Cursor 等本地 AI 客户端以子进程方式拉起本程序，
//! stdin/stdout 走 newline-delimited JSON-RPC（stdout 只允许协议帧，日志全部走 stderr）。

use crate::jsonrpc;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub async fn serve(ctx: Arc<crate::ctx::Ctx>) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();
    eprintln!("[screenlink-mcp] stdio transport ready");
    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[screenlink-mcp] bad json: {e}");
                continue;
            }
        };
        if let Some(resp) = jsonrpc::handle(&ctx, &msg).await {
            let mut out = resp.to_string();
            out.push('\n');
            stdout.write_all(out.as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
