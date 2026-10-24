//! ScreenLink 桌面端入口：装配状态、WS 流桥、IPC 服务、设备轮询与事件转发。

mod commands;
mod state;

use state::{AppEvent, AppState};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_devices,
            commands::connect_addr,
            commands::disconnect_addr,
            commands::pair_addr,
            commands::enable_tcpip,
            commands::scan_lan,
            commands::start_mirror,
            commands::stop_mirror,
            commands::send_touch,
            commands::send_scroll,
            commands::send_text,
            commands::send_key,
            commands::device_clipboard,
            commands::push_files,
            commands::pull_file,
            commands::list_transfers,
            commands::cancel_transfer,
            commands::upload_frame,
            commands::write_screenshot,
            commands::get_settings,
            commands::set_settings,
            commands::set_clipboard_sync,
            commands::enable_ai,
            commands::disable_ai,
            commands::get_ai_info,
            commands::get_audit,
            commands::clear_audit,
        ])
        .setup(move |app| {
            // 在 setup 内创建状态：此时 tauri 的 tokio runtime 已就绪
            // （TransferManager 等组件依赖 tokio::spawn）
            let st = Arc::new(
                AppState::new()
                    .map_err(|e| format!("初始化失败（未找到 adb？）: {e:#}"))?,
            );
            app.manage(st.clone());
            let handle = app.handle().clone();
            spawn_background(st, handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("ScreenLink 启动失败");
}

fn spawn_background(st: Arc<AppState>, handle: AppHandle) {
    // 应用事件总线：核心 -> UI
    let (app_tx, mut app_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    *st.app_tx.lock().unwrap() = Some(app_tx);

    tauri::async_runtime::spawn(async move {
        let mut transfers_rx = st.transfers.subscribe();
        let mut clip_rx = st.clip_rx.lock().unwrap().take();

        // WebSocket 流桥（视频 -> webview）
        {
            let sessions = st.sessions.clone();
            tokio::spawn(async move {
                let _ = screenlink_core::ws::serve(sessions, state::WS_PORT).await;
            });
        }

        // IPC 服务（MCP 快速通道）
        {
            let st2 = st.clone();
            let h2 = handle.clone();
            tokio::spawn(async move {
                let handler = commands::ipc_handler(st2, h2);
                let _ = screenlink_core::ipc::serve(handler).await;
            });
        }

        // 设备轮询（2s），并处理拔线自动 WiFi 重连
        {
            let st2 = st.clone();
            let h2 = handle.clone();
            tokio::spawn(async move {
                let mut prev: Vec<String> = Vec::new();
                loop {
                    let devices = commands::enrich(&st2.adb).await.unwrap_or_default();
                    let online: Vec<String> = devices
                        .iter()
                        .filter(|d| d.state == screenlink_core::device::DeviceState::Online)
                        .map(|d| d.serial.clone())
                        .collect();
                    if online != prev {
                        let _ = h2.emit("devices", &devices);
                        // 拔线自动切换：消失的 USB 设备尝试 wifi_targets 重连
                        let gone: Vec<String> = prev
                            .iter()
                            .filter(|s| !online.contains(s))
                            .cloned()
                            .collect();
                        let targets = st2.settings().wifi_targets.clone();
                        let auto = st2.settings().auto_switch_wifi;
                        for s in gone {
                            if let Some(addr) = targets.get(&s) {
                                if auto {
                                    let adb = st2.adb.clone();
                                    let addr = addr.clone();
                                    let s2 = s.clone();
                                    let h3 = h2.clone();
                                    tokio::spawn(async move {
                                        for _ in 0..3 {
                                            tokio::time::sleep(std::time::Duration::from_millis(
                                                1200,
                                            ))
                                            .await;
                                            if adb.connect(&addr).await.is_ok() {
                                                let _ = h3.emit(
                                                    "wifi-reconnected",
                                                    serde_json::json!(
                                                        { "serial": s2, "addr": addr }
                                                    ),
                                                );
                                                break;
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        prev = online;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            });
        }

        // 剪贴板双向同步泵：
        //  手机 -> PC：会话事件（DEVICE_CLIPBOARD）经 clip_rx 到达 -> 写本机剪贴板
        //  PC -> 手机：本机剪贴板线程经 local_rx 到达 -> SET_CLIPBOARD 到活跃会话
        {
            let st2 = st.clone();
            let h2 = handle.clone();
            tokio::spawn(async move {
                use screenlink_core::scrcpy::ControlMessage;
                let mut sync: Option<screenlink_core::clipboard::ClipboardSync> = None;
                let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                loop {
                    tokio::select! {
                        item = async { clip_rx.as_mut().unwrap().recv().await }, if clip_rx.is_some() => {
                            match item {
                                Some((serial, text)) => {
                                    if let Some(s) = &sync {
                                        s.apply_remote(&text);
                                    }
                                    let _ = h2.emit("device-clipboard", serde_json::json!({ "serial": serial, "text": text }));
                                }
                                None => break,
                            }
                        }
                        text = local_rx.recv() => {
                            if let Some(text) = text {
                                // 发到当前活跃投屏会话
                                let target = st2.sessions.lock().unwrap().values().next().cloned();
                                if let Some(h) = target {
                                    let _ = h
                                        .ctl
                                        .send(ControlMessage::SetClipboard {
                                            sequence: 0,
                                            text,
                                            paste: false,
                                        })
                                        .await;
                                }
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                            // 跟随设置开关启停本机剪贴板监听
                            let want = st2.settings().clipboard_sync;
                            match (&sync, want) {
                                (None, true) => {
                                    sync = Some(screenlink_core::clipboard::ClipboardSync::start(
                                        local_tx.clone(),
                                    ));
                                }
                                (Some(_), false) => sync = None,
                                _ => {}
                            }
                        }
                    }
                }
            });
        }

        // 事件转发循环
        loop {
            tokio::select! {
                ev = app_rx.recv() => {
                    match ev {
                        Some(AppEvent::DevicesChanged) => {
                            let devices = commands::enrich(&st.adb).await.unwrap_or_default();
                            let _ = handle.emit("devices", &devices);
                        }
                        Some(AppEvent::TransferSnapshots(v)) => {
                            let _ = handle.emit("transfers", &v);
                        }
                        None => break,
                    }
                }
                Ok(()) = transfers_rx.changed() => {
                    let _ = handle.emit("transfers", transfers_rx.borrow().clone());
                }
                else => break,
            }
        }
    });
}
