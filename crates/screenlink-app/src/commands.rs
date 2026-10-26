//! Tauri 命令层：UI <-> 核心逻辑的全部入口（核心逻辑都在 screenlink-core，本层只做编排）。

use crate::state::{AppEvent, AppState, FrameBlob, WS_PORT};
use screenlink_core::device::{DeviceState, Transport};
use screenlink_core::scrcpy::protocol::{and, ControlMessage};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

type Ctx<'a> = State<'a, Arc<AppState>>;

fn err(e: impl std::fmt::Display) -> Value {
    // 前端统一按 string 读取 message
    json!({ "error": e.to_string() })
}

// ---------- 设备与连接 ----------

#[tauri::command]
pub async fn list_devices(app: AppHandle, state: Ctx<'_>) -> Result<Value, Value> {
    let devices = enrich(&state.adb).await.unwrap_or_default();
    let mirroring: Vec<String> = state
        .sessions
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let _ = app.emit("devices", &devices);
    Ok(json!({ "devices": devices, "mirroring": mirroring }))
}

/// 设备列表 + 属性富化（型号/安卓版本/电量/wifi ip），失败字段留空。
pub async fn enrich(adb: &screenlink_core::adb::Adb) -> anyhow::Result<Vec<screenlink_core::device::DeviceInfo>> {
    let mut devices = adb.devices().await?;
    let mut jobs = Vec::new();
    for d in devices.clone() {
        if d.state != DeviceState::Online {
            continue;
        }
        jobs.push(tokio::spawn({
            let adb = adb.clone();
            async move {
                let props = adb
                    .shell(&d.serial, "getprop ro.product.manufacturer; getprop ro.product.model; getprop ro.build.version.release; getprop ro.build.version.sdk")
                    .await
                    .unwrap_or_default();
                let mut it = props.lines().map(str::trim);
                let manufacturer = it.next().unwrap_or("").to_string();
                let model = it.next().unwrap_or("").to_string();
                let android_version = it.next().unwrap_or("").to_string();
                let sdk = it.next().unwrap_or("").to_string();
                let battery = adb.battery(&d.serial).await;
                let wifi_ip = if d.transport == Transport::Wifi {
                    d.serial.split(':').next().map(String::from)
                } else {
                    adb.wifi_ip(&d.serial).await
                };
                (d.serial, manufacturer, model, android_version, sdk, battery, wifi_ip)
            }
        }));
    }
    for job in jobs {
        if let Ok((serial, manufacturer, model, android_version, sdk, battery, wifi_ip)) =
            job.await
        {
            if let Some(d) = devices.iter_mut().find(|d| d.serial == serial) {
                d.manufacturer = manufacturer;
                if !model.is_empty() {
                    d.model = model.replace('_', " ");
                }
                d.android_version = android_version;
                d.sdk = sdk;
                d.battery = battery;
                d.wifi_ip = wifi_ip;
            }
        }
    }
    Ok(devices)
}

#[tauri::command]
pub async fn connect_addr(state: Ctx<'_>, addr: String) -> Result<Value, Value> {
    state
        .adb
        .connect(&addr)
        .await
        .map(|msg| json!({ "message": msg }))
        .map_err(|e| err(e))
}

#[tauri::command]
pub async fn disconnect_addr(state: Ctx<'_>, addr: String) -> Result<(), Value> {
    state.adb.disconnect(&addr).await.map_err(|e| err(e))
}

#[tauri::command]
pub async fn pair_addr(state: Ctx<'_>, addr: String, code: String) -> Result<Value, Value> {
    state
        .adb
        .pair(&addr, &code)
        .await
        .map(|msg| json!({ "message": msg }))
        .map_err(|e| err(e))
}

/// USB -> WiFi：tcpip 5555 + 记住地址，供拔线自动重连。
#[tauri::command]
pub async fn enable_tcpip(app: AppHandle, state: Ctx<'_>, serial: String) -> Result<Value, Value> {
    state
        .adb
        .tcpip(&serial, 5555)
        .await
        .map_err(|e| err(e))?;
    // 等 tcpip 生效后查询 wlan0 IP
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let ip = state.adb.wifi_ip(&serial).await;
    if let Some(ip) = &ip {
        state.update_settings(|s| {
            s.wifi_targets.insert(serial.clone(), format!("{ip}:5555"));
        });
        let _ = app.emit("wifi-ready", json!({ "serial": serial, "addr": format!("{ip}:5555") }));
    }
    Ok(json!({ "ip": ip, "addr": ip.map(|i| format!("{i}:5555")) }))
}

#[tauri::command]
pub async fn scan_lan(state: Ctx<'_>) -> Result<Value, Value> {
    let r = screenlink_core::discovery::Discovery::scan(&state.adb).await;
    Ok(serde_json::to_value(&r).unwrap_or(json!({})))
}

// ---------- 投屏与反控 ----------

#[tauri::command]
pub async fn start_mirror(app: AppHandle, state: Ctx<'_>, serial: String) -> Result<Value, Value> {
    let info = state.start_mirror(&serial).await.map_err(|e| err(e))?;
    let _ = app.emit("mirror-started", json!({ "serial": serial, "info": info }));
    Ok(json!({ "info": info, "wsPort": WS_PORT }))
}

#[tauri::command]
pub async fn stop_mirror(state: Ctx<'_>, serial: String) -> Result<(), Value> {
    state.stop_mirror(&serial);
    Ok(())
}

fn session_ctl<'a>(
    state: &'a Arc<AppState>,
    serial: &str,
) -> Result<Arc<screenlink_core::scrcpy::SessionHandle>, Value> {
    let s = state.sessions.lock().unwrap().get(serial).cloned();
    s.ok_or_else(|| err("设备未在投屏中"))
}

fn session_ctl_ipc(
    state: &Arc<AppState>,
    serial: &str,
) -> Result<Arc<screenlink_core::scrcpy::SessionHandle>, (i32, String)> {
    let s = state.sessions.lock().unwrap().get(serial).cloned();
    s.ok_or_else(|| (-32004, "设备未在投屏中".to_string()))
}

/// 鼠标/触摸注入（坐标为设备屏幕像素）。
#[tauri::command]
pub async fn send_touch(
    state: Ctx<'_>,
    serial: String,
    action: u8,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    action_button: i32,
    buttons: i32,
) -> Result<(), Value> {
    let s = session_ctl(&state, &serial)?;
    s.ctl
        .send(ControlMessage::InjectTouch {
            action,
            pointer_id: u64::MAX, // scrcpy 虚拟鼠标 pointer id = -1
            x,
            y,
            screen_w: w as u16,
            screen_h: h as u16,
            pressure: if action == and::ACTION_UP { 0 } else { 0xffff },
            action_button,
            buttons,
        })
        .await
        .map_err(|e| err(e))
}

#[tauri::command]
pub async fn send_scroll(
    state: Ctx<'_>,
    serial: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    dy: f64,
) -> Result<(), Value> {
    let s = session_ctl(&state, &serial)?;
    s.ctl
        .send(ControlMessage::InjectScroll {
            x,
            y,
            screen_w: w as u16,
            screen_h: h as u16,
            h_scroll: 0,
            v_scroll: screenlink_core::scrcpy::protocol::i16_fixed_point(-dy.signum() as f32),
            buttons: 0,
        })
        .await
        .map_err(|e| err(e))
}

#[tauri::command]
pub async fn send_text(state: Ctx<'_>, serial: String, text: String) -> Result<(), Value> {
    let s = session_ctl(&state, &serial)?;
    // INJECT_TEXT 上限 300B（UTF-8），超长按块拆分
    for chunk in split_utf8(&text, 280) {
        s.ctl
            .send(ControlMessage::InjectText(chunk))
            .await
            .map_err(|e| err(e))?;
    }
    Ok(())
}

fn split_utf8(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut len = 0usize;
    for ch in text.chars() {
        let l = ch.len_utf8();
        if len + l > max {
            out.push(std::mem::take(&mut cur));
            len = 0;
        }
        cur.push(ch);
        len += l;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 命名按键：back/home/enter/esc/up/down/left/right/vol_up/vol_down/power/menu/recents
#[tauri::command]
pub async fn send_key(state: Ctx<'_>, serial: String, key: String) -> Result<(), Value> {
    let s = session_ctl(&state, &serial)?;
    let code: i32 = match key.as_str() {
        "back" => and::KEYCODE_BACK,
        "home" => and::KEYCODE_HOME,
        "enter" => and::KEYCODE_ENTER,
        "esc" => and::KEYCODE_ESC,
        "backspace" => and::KEYCODE_DEL,
        "page_up" => and::KEYCODE_PAGE_UP,
        "page_down" => and::KEYCODE_PAGE_DOWN,
        "home_key" => and::KEYCODE_MOVE_HOME,
        "move_end" => and::KEYCODE_MOVE_END,
        "up" => and::KEYCODE_DPAD_UP,
        "down" => and::KEYCODE_DPAD_DOWN,
        "left" => and::KEYCODE_DPAD_LEFT,
        "right" => and::KEYCODE_DPAD_RIGHT,
        "vol_up" => and::KEYCODE_VOLUME_UP,
        "vol_down" => and::KEYCODE_VOLUME_DOWN,
        "power" => and::KEYCODE_POWER,
        "menu" => and::KEYCODE_MENU,
        "recents" => and::KEYCODE_APP_SWITCH,
        "tab" => and::KEYCODE_TAB,
        other => other.parse().map_err(|_| err(format!("未知按键 {other}")))?,
    };
    s.ctl
        .send(ControlMessage::InjectKeycode {
            action: and::ACTION_DOWN,
            keycode: code,
            meta: 0,
        })
        .await
        .map_err(|e| err(e))?;
    s.ctl
        .send(ControlMessage::InjectKeycode {
            action: and::ACTION_UP,
            keycode: code,
            meta: 0,
        })
        .await
        .map_err(|e| err(e))
}

/// Ctrl+C/Ctrl+V 的剪贴板路径：SET_CLIPBOARD(paste=true) 或 GET_CLIPBOARD(copy)。
#[tauri::command]
pub async fn device_clipboard(
    state: Ctx<'_>,
    serial: String,
    action: String,
    text: Option<String>,
) -> Result<Value, Value> {
    let s = session_ctl(&state, &serial)?;
    match action.as_str() {
        "set" => {
            let text = text.unwrap_or_default();
            s.ctl
                .send(ControlMessage::SetClipboard {
                    sequence: 1,
                    text,
                    paste: false,
                })
                .await
                .map_err(|e| err(e))?;
            Ok(json!({ "ok": true }))
        }
        "get" => {
            let cached = state
                .last_device_clip
                .lock()
                .unwrap()
                .get(&serial)
                .cloned();
            s.ctl
                .send(ControlMessage::GetClipboard(1)) // COPY 键触发 autosync 上报
                .await
                .map_err(|e| err(e))?;
            Ok(json!({ "text": cached.unwrap_or_default() }))
        }
        _ => Err(err("action 应为 set|get")),
    }
}

// ---------- 文件传输 ----------

/// 展开拖拽路径：目录递归为文件清单 + 相对路径（保留结构），文件保持原名。
/// 返回 (文件绝对路径列表, 对应远端相对路径)；上限 2000 个文件防误拖系统目录。
fn expand_push_paths(paths: &[String]) -> Result<(Vec<std::path::PathBuf>, Vec<String>), Value> {
    let mut files = Vec::new();
    let mut rels = Vec::new();
    fn walk(dir: &std::path::Path, prefix: &str, files: &mut Vec<std::path::PathBuf>, rels: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = format!("{prefix}/{name}");
            let p = e.path();
            if p.is_dir() {
                walk(&p, &rel, files, rels);
            } else if p.is_file() {
                files.push(p);
                rels.push(rel);
            }
        }
    }
    for raw in paths {
        let p = std::path::PathBuf::from(raw);
        let root = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        if p.is_dir() {
            walk(&p, &root, &mut files, &mut rels);
        } else if p.is_file() {
            files.push(p);
            rels.push(root);
        }
        if files.len() > 2000 {
            return Err(err("一次最多传输 2000 个文件"));
        }
    }
    if files.is_empty() {
        return Err(err("没有可传输的文件"));
    }
    Ok((files, rels))
}

#[tauri::command]
pub async fn push_files(
    state: Ctx<'_>,
    serial: String,
    paths: Vec<String>,
    remote_dir: String,
    policy: Option<String>,
) -> Result<Value, Value> {
    use screenlink_core::transfer::{ConflictPolicy, JobKind};
    let policy = match policy.as_deref() {
        Some("skip") => ConflictPolicy::Skip,
        Some("rename") => ConflictPolicy::Rename,
        _ => ConflictPolicy::Overwrite,
    };
    let (files, rels) = expand_push_paths(&paths)?;
    let id = state.transfers.enqueue(
        JobKind::Push,
        &serial,
        files,
        rels,
        &remote_dir,
        std::path::PathBuf::new(),
        policy,
    );
    Ok(json!({ "jobId": id }))
}

#[tauri::command]
pub async fn pull_file(
    state: Ctx<'_>,
    serial: String,
    remote_path: String,
    local_dir: String,
    policy: Option<String>,
) -> Result<Value, Value> {
    use screenlink_core::transfer::{ConflictPolicy, JobKind};
    let policy = match policy.as_deref() {
        Some("skip") => ConflictPolicy::Skip,
        Some("rename") => ConflictPolicy::Rename,
        _ => ConflictPolicy::Overwrite,
    };
    let dir = if local_dir.is_empty() {
        dirs_download()
    } else {
        std::path::PathBuf::from(local_dir)
    };
    let id = state.transfers.enqueue(
        JobKind::Pull,
        &serial,
        vec![],
        vec![],
        &remote_path,
        dir,
        policy,
    );
    Ok(json!({ "jobId": id }))
}

/// 失败任务重试：以原参数重新入队。
#[tauri::command]
pub async fn retry_transfer(state: Ctx<'_>, id: u64) -> Result<Value, Value> {
    state
        .transfers
        .retry(id)
        .map(|nid| json!({ "jobId": nid }))
        .ok_or_else(|| err("任务不存在或不可重试"))
}

// ---------- USB 驱动识别与修复（老电脑场景） ----------

#[tauri::command]
pub async fn usb_driver_status(_state: Ctx<'_>) -> Result<Value, Value> {
    let st = tokio::task::spawn_blocking(screenlink_core::usb::status)
        .await
        .map_err(|e| err(e))?
        .map_err(|e| err(e))?;
    serde_json::to_value(st).map_err(|e| err(e))
}

/// 下载 Google USB Driver 并提权安装（触发 UAC，耗时约 10-30s）。
#[tauri::command]
pub async fn usb_driver_install(_state: Ctx<'_>) -> Result<Value, Value> {
    let r = tokio::task::spawn_blocking(screenlink_core::usb::install_google_usb_driver)
        .await
        .map_err(|e| err(e))?;
    r.map(|message| json!({ "message": message })).map_err(|e| err(e))
}

fn dirs_download() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            let p = std::path::PathBuf::from(home).join("Downloads");
            if p.is_dir() {
                return p;
            }
        }
    }
    std::path::PathBuf::from(".")
}

#[tauri::command]
pub async fn list_transfers(state: Ctx<'_>) -> Result<Value, Value> {
    Ok(serde_json::to_value(state.transfers.snapshots()).unwrap_or(json!([])))
}

#[tauri::command]
pub async fn cancel_transfer(state: Ctx<'_>, id: u64) -> Result<(), Value> {
    state.transfers.cancel(id).await;
    Ok(())
}

// ---------- 帧上传（UI canvas -> 核心，供 MCP 截图） ----------

#[tauri::command]
pub async fn upload_frame(
    state: Ctx<'_>,
    serial: String,
    mime: String,
    data_b64: String,
) -> Result<(), Value> {
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| err(e))?;
    let blob = FrameBlob {
        mime,
        data,
        ts: std::time::Instant::now(),
    };
    state.frame_cache.lock().unwrap().insert(serial.clone(), blob);
    let mut waiters = state.frame_waiters.lock().unwrap();
    if let Some(list) = waiters.remove(&serial) {
        let cache = state.frame_cache.lock().unwrap();
        if let Some(b) = cache.get(&serial) {
            for tx in list {
                let _ = tx.send(b.clone());
            }
        }
    }
    Ok(())
}

// ---------- 截图落盘（UI canvas -> 文件） ----------

#[tauri::command]
pub async fn write_screenshot(serial: String, data_b64: String) -> Result<Value, Value> {
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| err(e))?;
    let dir = screenlink_core::util::data_dir().join("screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| err(e))?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("screenlink-{serial}-{ms}.png"));
    std::fs::write(&path, &data).map_err(|e| err(e))?;
    Ok(json!({ "path": path.to_string_lossy() }))
}

// ---------- 设置 / 剪贴板同步 / AI ----------

#[tauri::command]
pub async fn get_settings(state: Ctx<'_>) -> Result<Value, Value> {
    let s = state.settings();
    Ok(json!({
        "settings": s,
        "aiEnabled": screenlink_core::auth::AiAccess::enabled(),
        "wsPort": WS_PORT,
    }))
}

#[tauri::command]
pub async fn set_settings(state: Ctx<'_>, settings: Value) -> Result<Value, Value> {
    let cur = state.settings();
    let new: crate::state::Settings = serde_json::from_value(settings).map_err(|e| err(e))?;
    // wifi_targets 是内部状态，不从 UI 覆盖
    let merged = crate::state::Settings { wifi_targets: cur.wifi_targets.clone(), ..new };
    *state.settings.lock().unwrap() = merged.clone();
    merged.save();
    Ok(json!({ "ok": true, "settings": merged }))
}

#[tauri::command]
pub async fn set_clipboard_sync(state: Ctx<'_>, on: bool) -> Result<(), Value> {
    // 实际的监听启停由 lib.rs 的泵跟随设置执行
    state.update_settings(|s| s.clipboard_sync = on);
    Ok(())
}

#[tauri::command]
pub async fn enable_ai(state: Ctx<'_>) -> Result<Value, Value> {
    let access = screenlink_core::auth::AiAccess::enable().map_err(|e| err(e))?;
    let snippet = screenlink_core::auth::AiAccess::mcp_snippet(&mcp_command());
    let _ = &state;
    Ok(json!({ "token": access.token, "snippet": snippet, "scopes": access.scopes }))
}

#[tauri::command]
pub async fn disable_ai(_state: Ctx<'_>) -> Result<(), Value> {
    screenlink_core::auth::AiAccess::revoke().map_err(|e| err(e))
}

#[tauri::command]
pub async fn get_ai_info(_state: Ctx<'_>) -> Result<Value, Value> {
    let enabled = screenlink_core::auth::AiAccess::enabled();
    let snippet = screenlink_core::auth::AiAccess::mcp_snippet(&mcp_command());
    Ok(json!({ "enabled": enabled, "snippet": snippet }))
}

fn mcp_command() -> String {
    // 开发期用 target/debug 绝对路径，安装后用 PATH 上的 screenlink-mcp
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    if let Some(dir) = exe_dir {
        let cand = dir.join(if cfg!(windows) { "screenlink-mcp.exe" } else { "screenlink-mcp" });
        if cand.is_file() {
            return cand.to_string_lossy().into_owned();
        }
    }
    "screenlink-mcp".into()
}

#[tauri::command]
pub async fn get_audit(_state: Ctx<'_>, limit: Option<usize>) -> Result<Value, Value> {
    Ok(json!({ "entries": screenlink_core::audit::tail(limit.unwrap_or(100)) }))
}

#[tauri::command]
pub async fn clear_audit(_state: Ctx<'_>) -> Result<(), Value> {
    screenlink_core::audit::clear().map_err(|e| err(e))
}

// ---------- IPC 请求处理（MCP 快速通道，GUI 侧后端） ----------

/// 组装 MCP IPC 的处理器：screenshot/tap/swipe/type/key/devices/page_state。
pub fn ipc_handler(state: Arc<AppState>, app: AppHandle) -> screenlink_core::ipc::Handler {
    Arc::new(move |method: String, params: Value| {
        let state = state.clone();
        let app = app.clone();
        Box::pin(async move {
            let serial = params
                .get("serial")
                .and_then(|s| s.as_str())
                .map(String::from);
            let auto_serial = || async {
                if let Some(s) = &serial {
                    return Ok(s.clone());
                }
                {
                    let sessions = state.sessions.lock().unwrap();
                    if let Some(k) = sessions.keys().next() {
                        return Ok(k.clone());
                    }
                }
                let devs = enrich(&state.adb).await.unwrap_or_default();
                devs.into_iter()
                    .find(|d| d.state == DeviceState::Online)
                    .map(|d| d.serial)
                    .ok_or_else(|| (-32004, "无在线设备".to_string()))
            };
            match method.as_str() {
                "ping" => Ok(json!({ "pong": true })),
                "devices" => {
                    let devices = enrich(&state.adb)
                        .await
                        .map_err(|e| (-32005, e.to_string()))?;
                    let mirroring: Vec<String> =
                        state.sessions.lock().unwrap().keys().cloned().collect();
                    Ok(json!({ "devices": devices, "mirroring": mirroring }))
                }
                "screenshot" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let t0 = std::time::Instant::now();
                    // 1) 缓存命中（<300ms）
                    {
                        let cache = state.frame_cache.lock().unwrap();
                        if let Some(b) = cache.get(&s) {
                            if b.ts.elapsed() < std::time::Duration::from_millis(300) {
                                use base64::Engine;
                                return Ok(json!({
                                    "mime": b.mime, "width": null, "height": null,
                                    "data": base64::engine::general_purpose::STANDARD.encode(&b.data),
                                    "ms": t0.elapsed().as_millis() as u64,
                                    "source": "cache",
                                }));
                            }
                        }
                    }
                    // 2) 向 webview 请求抓帧（<60ms 路径）
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    state
                        .frame_waiters
                        .lock()
                        .unwrap()
                        .entry(s.clone())
                        .or_default()
                        .push(tx);
                    let _ = app.emit("frame-request", json!({ "serial": s }));
                    if let Ok(Ok(b)) =
                        tokio::time::timeout(std::time::Duration::from_millis(250), rx).await
                    {
                        use base64::Engine;
                        return Ok(json!({
                            "mime": b.mime,
                            "data": base64::engine::general_purpose::STANDARD.encode(&b.data),
                            "ms": t0.elapsed().as_millis() as u64,
                            "source": "webview",
                        }));
                    }
                    // 3) 兜底：adb screencap
                    let png = state
                        .adb
                        .screencap_png(&s)
                        .await
                        .map_err(|e| (-32005, e.to_string()))?;
                    use base64::Engine;
                    Ok(json!({
                        "mime": "image/png",
                        "data": base64::engine::general_purpose::STANDARD.encode(&png),
                        "ms": t0.elapsed().as_millis() as u64,
                        "source": "adb",
                    }))
                }
                "tap" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let session = session_ctl_ipc(&state, &s)?;
                    let x = params.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    let y = params.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    let (w, h) = session_info_wh(&session);
                    for action in [and::ACTION_DOWN, and::ACTION_UP] {
                        session
                            .ctl
                            .send(ControlMessage::InjectTouch {
                                action,
                                pointer_id: u64::MAX,
                                x,
                                y,
                                screen_w: w as u16,
                                screen_h: h as u16,
                                pressure: if action == and::ACTION_UP { 0 } else { 0xffff },
                                action_button: and::BUTTON_PRIMARY,
                                buttons: and::BUTTON_PRIMARY,
                            })
                            .await
                            .map_err(|e| (-32005, e.to_string()))?;
                    }
                    Ok(json!({ "ok": true }))
                }
                "swipe" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let session = session_ctl_ipc(&state, &s)?;
                    let get = |k: &str| params.get(k).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    let (x1, y1, x2, y2) = (get("x1"), get("y1"), get("x2"), get("y2"));
                    let dur = params
                        .get("duration_ms")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(300)
                        .max(50) as u32;
                    let (w, h) = session_info_wh(&session);
                    let steps = (dur / 16).clamp(4, 60);
                    session
                        .ctl
                        .send(ControlMessage::InjectTouch {
                            action: and::ACTION_DOWN,
                            pointer_id: u64::MAX,
                            x: x1,
                            y: y1,
                            screen_w: w as u16,
                            screen_h: h as u16,
                            pressure: 0xffff,
                            action_button: and::BUTTON_PRIMARY,
                            buttons: and::BUTTON_PRIMARY,
                        })
                        .await
                        .map_err(|e| (-32005, e.to_string()))?;
                    for i in 1..=steps {
                        let t = i as f64 / steps as f64;
                        let _ = tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                        let _ = session
                            .ctl
                            .send(ControlMessage::InjectTouch {
                                action: and::ACTION_MOVE,
                                pointer_id: u64::MAX,
                                x: (x1 as f64 + (x2 - x1) as f64 * t) as i32,
                                y: (y1 as f64 + (y2 - y1) as f64 * t) as i32,
                                screen_w: w as u16,
                                screen_h: h as u16,
                                pressure: 0xffff,
                                action_button: and::BUTTON_PRIMARY,
                                buttons: and::BUTTON_PRIMARY,
                            })
                            .await;
                    }
                    session
                        .ctl
                        .send(ControlMessage::InjectTouch {
                            action: and::ACTION_UP,
                            pointer_id: u64::MAX,
                            x: x2,
                            y: y2,
                            screen_w: w as u16,
                            screen_h: h as u16,
                            pressure: 0,
                            action_button: and::BUTTON_PRIMARY,
                            buttons: 0,
                        })
                        .await
                        .map_err(|e| (-32005, e.to_string()))?;
                    Ok(json!({ "ok": true }))
                }
                "type_text" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let session = session_ctl_ipc(&state, &s)?;
                    let text = params.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    for chunk in split_utf8(&text, 280) {
                        session
                            .ctl
                            .send(ControlMessage::InjectText(chunk))
                            .await
                            .map_err(|e| (-32005, e.to_string()))?;
                    }
                    Ok(json!({ "ok": true }))
                }
                "key" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let session = session_ctl_ipc(&state, &s)?;
                    let key = params.get("key").and_then(|k| k.as_str()).unwrap_or("back");
                    let code: i32 = match key {
                        "back" => and::KEYCODE_BACK,
                        "home" => and::KEYCODE_HOME,
                        "enter" => and::KEYCODE_ENTER,
                        "esc" => and::KEYCODE_ESC,
                        "up" => and::KEYCODE_DPAD_UP,
                        "down" => and::KEYCODE_DPAD_DOWN,
                        "left" => and::KEYCODE_DPAD_LEFT,
                        "right" => and::KEYCODE_DPAD_RIGHT,
                        "vol_up" => and::KEYCODE_VOLUME_UP,
                        "vol_down" => and::KEYCODE_VOLUME_DOWN,
                        "power" => and::KEYCODE_POWER,
                        "menu" => and::KEYCODE_MENU,
                        "recents" => and::KEYCODE_APP_SWITCH,
                        other => other
                            .parse()
                            .map_err(|_| (-32006, format!("未知按键 {other}")))?,
                    };
                    for action in [and::ACTION_DOWN, and::ACTION_UP] {
                        session
                            .ctl
                            .send(ControlMessage::InjectKeycode { action, keycode: code, meta: 0 })
                            .await
                            .map_err(|e| (-32005, e.to_string()))?;
                    }
                    Ok(json!({ "ok": true }))
                }
                "page_state" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let xml = state
                        .adb
                        .ui_xml(&s)
                        .await
                        .map_err(|e| (-32005, e.to_string()))?;
                    let activity = state.adb.current_focus(&s).await.unwrap_or_default();
                    let elements = screenlink_core::ipc::page_elements_from_xml(&xml);
                    Ok(json!({ "activity": activity, "elements": elements }))
                }
                "clipboard_get" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let text = state
                        .last_device_clip
                        .lock()
                        .unwrap()
                        .get(&s)
                        .cloned()
                        .unwrap_or_default();
                    Ok(json!({ "text": text }))
                }
                "clipboard_set" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let session = session_ctl_ipc(&state, &s)?;
                    let text = params.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    session
                        .ctl
                        .send(ControlMessage::SetClipboard { sequence: 1, text, paste: false })
                        .await
                        .map_err(|e| (-32005, e.to_string()))?;
                    Ok(json!({ "ok": true }))
                }
                "push_file" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let path = params
                        .get("local_path")
                        .and_then(|p| p.as_str())
                        .ok_or_else(|| (-32006, "缺 local_path".into()))?;
                    let dir = params
                        .get("remote_dir")
                        .and_then(|p| p.as_str())
                        .unwrap_or("/sdcard/Download/ScreenLink");
                    state
                        .adb
                        .push(&s, &[std::path::PathBuf::from(path)], dir, screenlink_core::adb::CancelToken::new(), |_, _, _| {})
                        .await
                        .map_err(|e| (-32005, e.to_string()))?;
                    Ok(json!({ "ok": true, "remote": dir }))
                }
                "pull_file" => {
                    let s = auto_serial().await.map_err(|(c, m)| (c, m))?;
                    let remote = params
                        .get("remote_path")
                        .and_then(|p| p.as_str())
                        .ok_or_else(|| (-32006, "缺 remote_path".into()))?;
                    let dir = params
                        .get("local_dir")
                        .and_then(|p| p.as_str())
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(dirs_download);
                    state
                        .adb
                        .pull(&s, remote, &dir, screenlink_core::adb::CancelToken::new(), |_, _, _| {})
                        .await
                        .map_err(|e| (-32005, e.to_string()))?;
                    Ok(json!({ "ok": true, "local_dir": dir.to_string_lossy() }))
                }
                _ => Err((-32601, format!("未知 IPC 方法 {method}"))),
            }
        })
    })
}

fn session_info_wh(s: &screenlink_core::scrcpy::SessionHandle) -> (i32, i32) {
    (s.info.width, s.info.height)
}
