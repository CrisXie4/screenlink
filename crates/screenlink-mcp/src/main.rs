//! ScreenLink MCP Server —— 把手机能力暴露给 AI 客户端。
//!
//! 用法（由 GUI 生成的配置片段驱动）：
//!   screenlink-mcp --device auto --token <TOKEN>              # stdio（Claude Desktop/Cursor）
//!   screenlink-mcp --transport http --port 27191 --token <T>  # HTTP（远程客户端）
//!
//! 安全：token 必须与 GUI「启用 AI 访问」生成的 ai_access.json 匹配，
//! 撤销后所有请求立即被拒；所有工具调用写入审计日志。

mod ctx;
mod http;
mod jsonrpc;
mod stdio;

use ctx::Ctx;
use screenlink_core::adb::Adb;

fn arg_of(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let device = arg_of(&args, "--device").filter(|d| d != "auto");
    let token = arg_of(&args, "--token")
        .or_else(|| std::env::var("SCREENLINK_TOKEN").ok())
        .unwrap_or_default();
    let transport = arg_of(&args, "--transport").unwrap_or_else(|| "stdio".into());
    let port: u16 = arg_of(&args, "--port").and_then(|p| p.parse().ok()).unwrap_or(27191);

    if token.is_empty() {
        eprintln!(
            "[screenlink-mcp] 错误：缺少 --token。请在 ScreenLink GUI 中「启用 AI 访问」并使用生成的配置片段。"
        );
        std::process::exit(2);
    }
    if !screenlink_core::auth::AiAccess::verify(&token) {
        eprintln!(
            "[screenlink-mcp] 错误：token 无效或 AI 访问已被撤销。请在 ScreenLink GUI 中重新启用。"
        );
        std::process::exit(3);
    }

    let adb = match Adb::discover() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[screenlink-mcp] 错误：{e}");
            std::process::exit(4);
        }
    };
    let c = Ctx::new(adb, token, device);

    if transport == "http" {
        if let Err(e) = http::serve(std::sync::Arc::new(c), port).await {
            eprintln!("[screenlink-mcp] http 服务异常：{e:#}");
            std::process::exit(1);
        }
    } else {
        if let Err(e) = stdio::serve(std::sync::Arc::new(c)).await {
            eprintln!("[screenlink-mcp] stdio 服务异常：{e:#}");
            std::process::exit(1);
        }
    }
}
