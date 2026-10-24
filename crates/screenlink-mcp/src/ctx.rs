//! MCP Server 上下文：设备选择、IPC 快速通道、ADB 慢速兜底。
//!
//! ## 性能设计（验收指标）
//! - `screenshot` <60ms：GUI 在跑时走 IPC（投屏帧缓存 / webview 抓帧）；
//!   GUI 不在时退回 `adb exec-out screencap`（约 100-300ms，功能可用但慢）。
//! - 输入类 <20ms：GUI 在跑时复用其 Scrcpy 控制 socket；否则 `adb shell input`。

use anyhow::{anyhow, Result};
use screenlink_core::adb::Adb;
use screenlink_core::ipc::IpcClient;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct Ctx {
    pub adb: Adb,
    pub ipc: IpcClient,
    /// 默认设备 serial（auto = 首个投屏中/在线设备）
    pub device: RwLock<Option<String>>,
}

impl Ctx {
    pub fn new(adb: Adb, token: String, device: Option<String>) -> Self {
        Self {
            adb,
            ipc: IpcClient::new(token),
            device: RwLock::new(device),
        }
    }

    /// 解析目标设备：显式 serial > IPC/GUI 活跃会话 > 首个在线设备。
    async fn resolve_serial(&self, params: &Value) -> Result<String> {
        if let Some(s) = params.get("serial").and_then(|s| s.as_str()) {
            if !s.is_empty() {
                return Ok(s.to_string());
            }
        }
        if let Some(d) = self.device.read().await.as_ref() {
            return Ok(d.clone());
        }
        // GUI 活跃会话优先（输入走 <20ms 快速通道）
        if self.ipc.has_token() {
            if let Ok(v) = self.ipc.request("devices", json!({})).await {
                if let Some(m) = v.get("mirroring").and_then(|m| m.as_array()) {
                    if let Some(first) = m.first().and_then(|s| s.as_str()) {
                        return Ok(first.to_string());
                    }
                }
            }
        }
        let devs = self.adb.devices().await?;
        devs.into_iter()
            .find(|d| d.state == screenlink_core::device::DeviceState::Online)
            .map(|d| d.serial)
            .ok_or_else(|| anyhow!("无在线设备"))
    }

    /// 请求经 IPC（GUI 快速通道）；GUI 不在或未授权则返回 None 由调用方走 ADB。
    pub async fn ipc_try(&self, method: &str, params: Value) -> Option<Value> {
        if !self.ipc.has_token() {
            return None;
        }
        self.ipc.request(method, params).await.ok()
    }
}

// ---------- 工具实现（每个返回 MCP content 数组） ----------

pub async fn tool_screenshot(ctx: &Ctx, params: Value) -> Result<(Value, bool)> {
    let serial = ctx.resolve_serial(&params).await?;
    // 快路径：GUI IPC（投屏帧）
    if let Some(v) = ctx.ipc_try("screenshot", json!({ "serial": serial })).await {
        let mime = v.get("mime").and_then(|m| m.as_str()).unwrap_or("image/png").to_string();
        let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("").to_string();
        let ms = v.get("ms").and_then(|m| m.as_u64()).unwrap_or(0);
        return Ok((json!({
            "content": [{ "type": "image", "mimeType": mime, "data": data },
                        { "type": "text", "text": format!("截图耗时 {ms}ms (via GUI 投屏帧)") }]
        }), true));
    }
    // 慢路径：adb screencap
    let png = ctx.adb.screencap_png(&serial).await?;
    use base64::Engine;
    Ok((json!({
        "content": [{ "type": "image", "mimeType": "image/png",
                      "data": base64::engine::general_purpose::STANDARD.encode(&png) },
                    { "type": "text", "text": "截图来自 adb screencap（GUI 未运行，走慢速通道）" }]
    }), false))
}

pub async fn tool_tap(ctx: &Ctx, params: Value) -> Result<(Value, bool)> {
    let serial = ctx.resolve_serial(&params).await?;
    let x = params.get("x").and_then(|v| v.as_i64()).ok_or_else(|| anyhow!("缺 x"))?;
    let y = params.get("y").and_then(|v| v.as_i64()).ok_or_else(|| anyhow!("缺 y"))?;
    // 支持 index 点击：与 page_state 的元素索引配合
    if let Some(idx) = params.get("index").and_then(|v| v.as_u64()) {
        let state = page_state(ctx, &serial).await?;
        if let Some(el) = state.get("elements").and_then(|e| e.as_array()).and_then(|a| a.get(idx as usize)) {
            if let Some([cx, cy]) = el.get("center").and_then(|c| c.as_array()).map(|v| v.as_slice()) {
                let x = cx.get(0).and_then(|v| v.as_i64()).unwrap_or(x);
                let y = cy.get(0).and_then(|v| v.as_i64()).unwrap_or(y);
                return tap_xy(ctx, &serial, x, y).await;
            }
        }
        return Err(anyhow!("索引 {idx} 不存在，请先调用 screenlink_page_state"));
    }
    tap_xy(ctx, &serial, x, y).await
}

async fn tap_xy(ctx: &Ctx, serial: &str, x: i64, y: i64) -> Result<(Value, bool)> {
    if let Some(_) = ctx.ipc_try("tap", json!({ "serial": serial, "x": x, "y": y })).await {
        return Ok((json!({ "content": [{ "type": "text", "text": format!("已点击 ({x},{y}) [scrcpy 控制通道]") }] }), true));
    }
    ctx.adb.run(&["-s", &serial, "shell", "input", "tap", &x.to_string(), &y.to_string()]).await?;
    Ok((json!({ "content": [{ "type": "text", "text": format!("已点击 ({x},{y}) [adb shell input]") }] }), false))
}

pub async fn tool_swipe(ctx: &Ctx, params: Value) -> Result<(Value, bool)> {
    let serial = ctx.resolve_serial(&params).await?;
    let g = |k: &str| -> anyhow::Result<i64> {
        params.get(k).and_then(|v| v.as_i64()).ok_or_else(|| anyhow!("缺 {k}"))
    };
    let (x1, y1, x2, y2) = (g("x1")?, g("y1")?, g("x2")?, g("y2")?);
    let dur = params.get("duration_ms").and_then(|v| v.as_i64()).unwrap_or(300);
    if ctx.ipc_try("swipe", json!({ "serial": serial, "x1": x1, "y1": y1, "x2": x2, "y2": y2, "duration_ms": dur })).await.is_some() {
        return Ok((json!({ "content": [{ "type": "text", "text": "已滑动 [scrcpy 控制通道]" }] }), true));
    }
    ctx.adb.run(&["-s", &serial, "shell", "input", "swipe", &x1.to_string(), &y1.to_string(), &x2.to_string(), &y2.to_string(), &dur.to_string()]).await?;
    Ok((json!({ "content": [{ "type": "text", "text": "已滑动 [adb shell input]" }] }), false))
}

pub async fn tool_type(ctx: &Ctx, params: Value) -> Result<(Value, bool)> {
    let serial = ctx.resolve_serial(&params).await?;
    let text = params.get("text").and_then(|t| t.as_str()).ok_or_else(|| anyhow!("缺 text"))?.to_string();
    if ctx.ipc_try("type_text", json!({ "serial": serial, "text": text })).await.is_some() {
        return Ok((json!({ "content": [{ "type": "text", "text": "已输入文字 [scrcpy INJECT_TEXT]" }] }), true));
    }
    // adb input text 不支持空格/中文：空格转 %s，中文仅 scrcpy 通道可靠
    let escaped = text.replace(' ', "%s");
    ctx.adb.run(&["-s", &serial, "shell", "input", "text", &escaped]).await?;
    Ok((json!({ "content": [{ "type": "text", "text": "已输入文字 [adb shell input，中文请在 GUI 中投屏后使用]" }] }), false))
}

pub async fn tool_key(ctx: &Ctx, params: Value) -> Result<(Value, bool)> {
    let serial = ctx.resolve_serial(&params).await?;
    let key = params.get("key").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("缺 key"))?.to_string();
    let code: &str = match key.as_str() {
        "back" => "4", "home" => "3", "enter" => "66", "esc" => "111",
        "up" => "19", "down" => "20", "left" => "21", "right" => "22",
        "vol_up" => "24", "vol_down" => "25", "power" => "26", "menu" => "82", "recents" => "187",
        other => other,
    };
    if ctx.ipc_try("key", json!({ "serial": serial, "key": key })).await.is_some() {
        return Ok((json!({ "content": [{ "type": "text", "text": format!("已按键 {key} [scrcpy 控制通道]") }] }), true));
    }
    ctx.adb.run(&["-s", &serial, "shell", "input", "keyevent", code]).await?;
    Ok((json!({ "content": [{ "type": "text", "text": format!("已按键 {key} [adb shell input]") }] }), false))
}

pub async fn tool_devices(ctx: &Ctx) -> Result<(Value, bool)> {
    if let Some(v) = ctx.ipc_try("devices", json!({})).await {
        return Ok((json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v)? }] }), true));
    }
    let devs = ctx.adb.devices().await?;
    Ok((json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&devs)? }] }), false))
}

pub async fn tool_push(ctx: &Ctx, params: Value) -> Result<(Value, bool)> {
    let serial = ctx.resolve_serial(&params).await?;
    let local = params.get("local_path").and_then(|p| p.as_str()).ok_or_else(|| anyhow!("缺 local_path"))?;
    let remote_dir = params.get("remote_dir").and_then(|p| p.as_str()).unwrap_or("/sdcard/Download/ScreenLink");
    if let Some(_) = ctx.ipc_try("push_file", json!({ "serial": serial, "local_path": local, "remote_dir": remote_dir })).await {
        return Ok((json!({ "content": [{ "type": "text", "text": format!("已推送 {local} -> {remote_dir}") }] }), true));
    }
    let names = ctx.adb.push(&serial, &[std::path::PathBuf::from(local)], remote_dir, screenlink_core::adb::CancelToken::new(), |_, _, _| {}).await?;
    Ok((json!({ "content": [{ "type": "text", "text": format!("已推送到 {:?}", names) }] }), false))
}

pub async fn tool_pull(ctx: &Ctx, params: Value) -> Result<(Value, bool)> {
    let serial = ctx.resolve_serial(&params).await?;
    let remote = params.get("remote_path").and_then(|p| p.as_str()).ok_or_else(|| anyhow!("缺 remote_path"))?;
    let local_dir = params.get("local_dir").and_then(|p| p.as_str()).map(std::path::PathBuf::from);
    if let Some(_) = ctx.ipc_try("pull_file", json!({ "serial": serial, "remote_path": remote, "local_dir": local_dir.as_ref().map(|d| d.to_string_lossy()) })).await {
        return Ok((json!({ "content": [{ "type": "text", "text": format!("已拉取 {remote}") }] }), true));
    }
    let dir = local_dir.unwrap_or_else(default_download);
    let files = ctx.adb.pull(&serial, remote, &dir, screenlink_core::adb::CancelToken::new(), |_, _, _| {}).await?;
    Ok((json!({ "content": [{ "type": "text", "text": format!("已拉取到 {:?}", files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>()) }] }), false))
}

fn default_download() -> std::path::PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(|h| std::path::PathBuf::from(h).join("Downloads"))
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

pub async fn tool_page_state(ctx: &Ctx, params: Value) -> Result<(Value, bool)> {
    let serial = ctx.resolve_serial(&params).await?;
    let state = page_state(ctx, &serial).await?;
    Ok((json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&state)? }] }), false))
}

async fn page_state(ctx: &Ctx, serial: &str) -> Result<Value> {
    let xml = ctx.adb.ui_xml(serial).await?;
    let activity = ctx.adb.current_focus(serial).await.unwrap_or_default();
    let elements = screenlink_core::ipc::page_elements_from_xml(&xml);
    Ok(json!({
        "serial": serial,
        "activity": activity,
        "element_count": elements.len(),
        "elements": elements,
        "hint": "用 screenlink_tap 的 index 参数点击元素 center 坐标",
    }))
}
