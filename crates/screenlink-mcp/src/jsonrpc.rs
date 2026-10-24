//! MCP 协议层：JSON-RPC 2.0 封包 + initialize/tools/resources 方法。
//!
//! 协议版本：2025-06-18（兼容 2024-11-05 客户端，按客户端请求版本回显或取我们支持的最新版）。

use crate::ctx::{self, Ctx};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "screenlink";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 工具定义（名称 / 描述 / 参数 schema）—— AI 客户端据此自动发现能力。
fn tool_defs() -> Value {
    json!({
        "tools": [
            {
                "name": "screenlink_screenshot",
                "description": "获取手机当前屏幕截图（GUI 投屏运行时延迟 <60ms，返回图像）",
                "inputSchema": { "type": "object", "properties": {
                    "serial": { "type": "string", "description": "设备 serial，省略则用默认设备" } } }
            },
            {
                "name": "screenlink_tap",
                "description": "点击手机屏幕指定坐标，或点击 page_state 返回的元素 index",
                "inputSchema": { "type": "object", "properties": {
                    "x": { "type": "integer", "description": "屏幕 X 坐标（设备像素）" },
                    "y": { "type": "integer", "description": "屏幕 Y 坐标" },
                    "index": { "type": "integer", "description": "或：page_state 元素索引（优先于 x/y）" },
                    "serial": { "type": "string" } } }
            },
            {
                "name": "screenlink_swipe",
                "description": "在手机屏幕上滑动",
                "inputSchema": { "type": "object", "properties": {
                    "x1": { "type": "integer" }, "y1": { "type": "integer" },
                    "x2": { "type": "integer" }, "y2": { "type": "integer" },
                    "duration_ms": { "type": "integer", "default": 300 },
                    "serial": { "type": "string" } },
                    "required": ["x1", "y1", "x2", "y2"] }
            },
            {
                "name": "screenlink_type",
                "description": "向手机当前输入框输入文字",
                "inputSchema": { "type": "object", "properties": {
                    "text": { "type": "string" }, "serial": { "type": "string" } },
                    "required": ["text"] }
            },
            {
                "name": "screenlink_key",
                "description": "按键：back/home/enter/esc/up/down/left/right/vol_up/vol_down/power/menu/recents",
                "inputSchema": { "type": "object", "properties": {
                    "key": { "type": "string" }, "serial": { "type": "string" } },
                    "required": ["key"] }
            },
            {
                "name": "screenlink_devices",
                "description": "列出已连接的 Android 设备（USB + WiFi），含投屏状态",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "screenlink_push_file",
                "description": "推送电脑文件到手机（默认 Download/ScreenLink）",
                "inputSchema": { "type": "object", "properties": {
                    "local_path": { "type": "string" },
                    "remote_dir": { "type": "string", "default": "/sdcard/Download/ScreenLink" },
                    "serial": { "type": "string" } },
                    "required": ["local_path"] }
            },
            {
                "name": "screenlink_pull_file",
                "description": "从手机拉取文件到电脑",
                "inputSchema": { "type": "object", "properties": {
                    "remote_path": { "type": "string" },
                    "local_dir": { "type": "string", "description": "默认系统下载目录" },
                    "serial": { "type": "string" } },
                    "required": ["remote_path"] }
            },
            {
                "name": "screenlink_page_state",
                "description": "获取当前页面结构：前台 Activity + 可交互元素列表（文本/坐标/索引），供 AI 理解屏幕内容并按 index 点击",
                "inputSchema": { "type": "object", "properties": {
                    "serial": { "type": "string" } } }
            },
        ]
    })
}

fn resource_defs() -> Value {
    json!({
        "resources": [
            {
                "uri": "screenlink://device/{serial}/screen",
                "name": "手机实时屏幕画面",
                "description": "读取返回当前屏幕截图（PNG/JPEG），serial 省略部分用默认设备",
                "mimeType": "image/png",
            }
        ]
    })
}

pub struct HandleError(pub i32, pub String);

impl From<anyhow::Error> for HandleError {
    fn from(e: anyhow::Error) -> Self {
        HandleError(-32000, e.to_string())
    }
}

/// 处理一条 JSON-RPC 请求，返回响应（通知返回 None）。
pub async fn handle(ctx: &Arc<Ctx>, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();

    // 通知（无 id）：不响应
    let id = match id {
        Some(id) if !id.is_null() => id,
        _ => return None,
    };

    let result = dispatch(ctx, &method, msg.get("params").cloned().unwrap_or(json!({}))).await;
    Some(match result {
        Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
        Err(HandleError(code, message)) =>
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

async fn dispatch(ctx: &Arc<Ctx>, method: &str, params: Value) -> Result<Value, HandleError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        })),
        "notifications/initialized" | "initialized" => Ok(json!({})),
        "ping" => Ok(json!({})),
        "tools/list" => {
            let mut defs = tool_defs();
            if let Some(extra) = params.get("_defs") {
                defs["_extra"] = extra.clone();
            }
            Ok(defs)
        }
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let t0 = Instant::now();
            let (result, fast) = call_tool(ctx, name, args).await?;
            // 审计：所有 AI 操作落盘（GUI 审计面板可视化）
            let entry = screenlink_core::audit::AuditEntry {
                ts: screenlink_core::audit::now_iso(),
                client: "mcp".into(),
                tool: name.to_string(),
                device: ctx.device.read().await.clone().unwrap_or_else(|| "auto".into()),
                args: params.get("arguments").cloned().unwrap_or(json!({})),
                ok: true,
                detail: None,
                took_ms: t0.elapsed().as_millis(),
            };
            let _ = screenlink_core::audit::append(&entry);
            let _ = fast;
            Ok(json!({
                "content": result.get("content").cloned().unwrap_or(json!([])),
                "fastPath": fast,
            }))
        }
        "resources/list" => Ok(resource_defs()),
        "resources/read" => {
            let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            let serial = uri
                .strip_prefix("screenlink://device/")
                .and_then(|rest| rest.strip_suffix("/screen"))
                .filter(|s| !s.is_empty())
                .map(String::from);
            let mut p = json!({ "serial": serial });
            if serial.is_none() {
                p.as_object_mut().unwrap().remove("serial");
            }
            let (shot, _) = ctx::tool_screenshot(ctx, p).await?;
            let content = shot.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default();
            let img = content.iter().find(|c| c.get("type").and_then(|t| t.as_str()) == Some("image"));
            match img {
                Some(img) => Ok(json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": img.get("mimeType").cloned().unwrap_or(json!("image/png")),
                        "blob": img.get("data").cloned().unwrap_or(json!("")),
                    }]
                })),
                None => Err(HandleError(-32002, "截图失败".into())),
            }
        }
        _ => Err(HandleError(-32601, format!("未知方法 {method}"))),
    }
}

async fn call_tool(ctx: &Arc<Ctx>, name: &str, args: Value) -> Result<(Value, bool), HandleError> {
    let r = match name {
        "screenlink_screenshot" => ctx::tool_screenshot(ctx, args).await?,
        "screenlink_tap" => ctx::tool_tap(ctx, args).await?,
        "screenlink_swipe" => ctx::tool_swipe(ctx, args).await?,
        "screenlink_type" => ctx::tool_type(ctx, args).await?,
        "screenlink_key" => ctx::tool_key(ctx, args).await?,
        "screenlink_devices" => ctx::tool_devices(ctx).await?,
        "screenlink_push_file" => ctx::tool_push(ctx, args).await?,
        "screenlink_pull_file" => ctx::tool_pull(ctx, args).await?,
        "screenlink_page_state" => ctx::tool_page_state(ctx, args).await?,
        other => return Err(HandleError(-32602, format!("未知工具 {other}"))),
    };
    Ok(r)
}
