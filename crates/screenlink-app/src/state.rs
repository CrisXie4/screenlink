//! 应用全局状态：ADB、投屏会话表、传输队列、设置、帧缓存、剪贴板同步、AI 鉴权。

use screenlink_core::adb::Adb;
use screenlink_core::scrcpy::{DeviceEvent, SessionHandle, SessionStateInner};
use screenlink_core::transfer::TransferManager;
use screenlink_core::ws::SessionMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

pub const WS_PORT: u16 = 27193;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 视频上限边长（0 = 原生）
    pub max_size: i32,
    /// 视频码率 bps
    pub bit_rate: i32,
    pub max_fps: f32,
    /// 关键帧间隔（秒）：1 = 中途接入秒级恢复；低配设备可放宽到 4 降负载
    pub keyframe_interval: i32,
    /// 推送默认目录
    pub push_dir: String,
    /// 拉取默认目录（空 = 下载目录）
    pub pull_dir: String,
    /// 剪贴板双向同步
    pub clipboard_sync: bool,
    /// 投屏音频转发（opus；设备需 Android 11+，低版本自动降级为无音频）
    pub audio: bool,
    /// 拔线后自动尝试 WiFi 重连
    pub auto_switch_wifi: bool,
    /// serial -> 已知 WiFi 地址（自动重连用）
    pub wifi_targets: HashMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_size: 1600,
            bit_rate: 12_000_000,
            max_fps: 60.0,
            keyframe_interval: 1,
            push_dir: "/sdcard/Download/ScreenLink".into(),
            pull_dir: String::new(),
            clipboard_sync: true,
            // 音频转发默认关：手机媒体声音不一定要在电脑播放，设置/工具栏可开
            audio: false,
            auto_switch_wifi: true,
            wifi_targets: HashMap::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let p = screenlink_core::util::data_dir().join("settings.json");
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) {
        let p = screenlink_core::util::data_dir().join("settings.json");
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }
}

#[derive(Debug, Clone)]
pub struct FrameBlob {
    pub mime: String,
    pub data: Vec<u8>,
    pub ts: std::time::Instant,
}

pub struct AppState {
    pub adb: Adb,
    pub sessions: SessionMap,
    pub transfers: TransferManager,
    pub settings: Mutex<Settings>,
    /// 供 MCP IPC 快速截图：<300ms 内的最新帧缓存
    pub frame_cache: Mutex<HashMap<String, FrameBlob>>,
    /// 待响应的抓帧请求（UI 上传后 resolve）
    pub frame_waiters: Mutex<HashMap<String, Vec<tokio::sync::oneshot::Sender<FrameBlob>>>>,
    /// 剪贴板同步句柄（启用时存在）
    pub clip_sync: Mutex<Option<screenlink_core::clipboard::ClipboardSync>>,
    /// 手机 -> PC 剪贴板接收端（由会话事件泵投递）
    pub clip_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<(String, String)>>>,
    pub clip_tx: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    /// 最后一次从设备同步来的剪贴板（供 MCP clipboard_get）
    pub last_device_clip: Mutex<HashMap<String, String>>,
    pub app_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<AppEvent>>>,
    /// 进行中的伴侣 App 配对会话（单会话；begin→submit 同一 TCP 连接）
    pub pair_session: Mutex<Option<screenlink_core::agent_client::PairingSession>>,
    /// 最近一次扫描发现的伴侣 App 信标（serial|ip -> 回复），enrich 时给设备打 has_app 标记
    pub phone_beacons: Mutex<HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    DevicesChanged,
    TransferSnapshots(Vec<screenlink_core::transfer::JobSnapshot>),
}

impl AppState {
    pub fn new() -> anyhow::Result<Self> {
        let adb = Adb::discover()?;
        let sessions: SessionMap = std::sync::Arc::new(Mutex::new(HashMap::new()));
        let transfers = TransferManager::new(adb.clone());
        let (clip_tx, clip_rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            adb,
            sessions,
            transfers,
            settings: Mutex::new(Settings::load()),
            frame_cache: Mutex::new(HashMap::new()),
            frame_waiters: Mutex::new(HashMap::new()),
            clip_sync: Mutex::new(None),
            clip_rx: Mutex::new(Some(clip_rx)),
            clip_tx,
            last_device_clip: Mutex::new(HashMap::new()),
            app_tx: Mutex::new(None),
            pair_session: Mutex::new(None),
            phone_beacons: Mutex::new(HashMap::new()),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) -> Settings {
        let mut s = self.settings.lock().unwrap();
        f(&mut s);
        s.save();
        s.clone()
    }

    /// 启动投屏（含会话监督与剪贴板事件泵）。
    pub async fn start_mirror(
        self: &std::sync::Arc<Self>,
        serial: &str,
    ) -> anyhow::Result<(screenlink_core::scrcpy::VideoInfo, bool)> {
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(s) = sessions.get(serial) {
                if s.is_active() {
                    anyhow::bail!("该设备已在投屏中");
                }
            }
        }
        let jar = screenlink_core::scrcpy::session::locate_server_jar()?;
        let st = self.settings();
        // 音频需 Android 11（SDK 30）+；低版本强开会导致 server 启动失败，自动降级
        let mut audio = st.audio;
        if audio {
            let sdk = self
                .adb
                .shell(serial, "getprop ro.build.version.sdk")
                .await
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok());
            if sdk.is_some_and(|s| s < 30) {
                audio = false;
            }
        }
        let opts = screenlink_core::scrcpy::ServerOptions {
            scid: std::process::id() as u32 & 0x7fff_ffff,
            max_size: st.max_size,
            bit_rate: st.bit_rate,
            max_fps: st.max_fps,
            keyframe_interval: st.keyframe_interval.max(1),
            audio,
            stay_awake: true,
            show_touches: false,
        };
        let handle = screenlink_core::scrcpy::start_session(&self.adb, serial, opts, &jar).await?;
        let handle = std::sync::Arc::new(handle);
        let info = handle.info.clone();

        // 会话结束监督 -> 通知 UI
        let (app, h2, serial2) = (self.clone(), handle.clone(), serial.to_string());
        tokio::spawn(async move {
            let mut state_rx = h2.state_rx.clone();
            let mut ended = None;
            loop {
                if let SessionStateInner::Ended(r) = &*state_rx.borrow() {
                    ended = Some(r.clone());
                    break;
                }
                if state_rx.changed().await.is_err() {
                    break;
                }
            }
            app.sessions.lock().unwrap().remove(&serial2);
            if let Some(tx) = app.app_tx.lock().unwrap().as_ref() {
                let _ = tx.send(AppEvent::DevicesChanged);
            }
            if let Some(reason) = ended {
                tracing::info!("mirror ended: {serial2}: {reason}");
            }
        });

        // 设备事件泵（剪贴板 -> PC）
        let (app, h3, serial3) = (self.clone(), handle.clone(), serial.to_string());
        tokio::spawn(async move {
            let mut events = h3.events_rx.resubscribe();
            loop {
                match events.recv().await {
                    Ok(DeviceEvent::Clipboard(text)) => {
                        app.last_device_clip
                            .lock()
                            .unwrap()
                            .insert(serial3.clone(), text.clone());
                        let _ = app.clip_tx.send((serial3.clone(), text));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(_) => continue,
                }
            }
        });

        let audio_enabled = handle.audio_enabled;
        self.sessions
            .lock()
            .unwrap()
            .insert(serial.to_string(), handle);
        Ok((info, audio_enabled))
    }

    pub fn stop_mirror(&self, serial: &str) {
        let handle = self.sessions.lock().unwrap().get(serial).cloned();
        if let Some(h) = handle {
            let h = h.clone();
            tokio::spawn(async move { h.stop().await });
        }
    }
}
