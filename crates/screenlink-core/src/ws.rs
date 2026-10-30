//! WebSocket 流桥：把投屏会话的视频块推给 webview 渲染端（WebCodecs 解码）。
//!
//! 路由：`ws://127.0.0.1:<port>/mirror/{serial}`
//! 消息即 scrcpy/session.rs 定义的 9B 头 + H.264 负载（Annex-B）。
//! 选 WebSocket 而非 Tauri IPC 的原因：连续二进制流无 JSON/拷贝开销，本地回环 <1ms。

use crate::scrcpy::SessionHandle;
use anyhow::Result;
type WsStream = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};

pub type SessionMap = Arc<Mutex<HashMap<String, Arc<SessionHandle>>>>;

pub async fn serve(sessions: SessionMap, port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!("mirror WS bridge on 127.0.0.1:{port}");
    loop {
        let (stream, _) = listener.accept().await?;
        let sessions = sessions.clone();
        tokio::spawn(async move {
            let _ = handle(stream, sessions).await;
        });
    }
}

async fn handle(stream: TcpStream, sessions: SessionMap) -> Result<()> {
    let mut ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("[ws] handshake failed: {e}");
            return Ok(());
        }
    };
    // 客户端首条文本消息 = "mirror:<serial>"（避免依赖 HTTP upgrade path 的解析差异）
    let serial = match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
        Ok(Some(Ok(msg))) => {
            let text = msg.into_text().unwrap_or_default();
            text.strip_prefix("mirror:").unwrap_or("").to_string()
        }
        _ => {
            tracing::warn!("[ws] 未收到 mirror 问候，关闭");
            return Ok(());
        }
    };
    if serial.is_empty() {
        return Ok(());
    }
    let session = sessions.lock().unwrap().get(&serial).cloned();
    let Some(session) = session else {
        tracing::warn!("[ws] 找不到会话 {serial}");
        let _ = ws.send(tokio_tungstenite::tungstenite::Message::text("error:no-session")).await;
        return Ok(());
    };
    tracing::info!("[ws] {serial} 客户端接入");

    // 补发缓存的最后关键帧：静态画面下可能数秒无新帧，且会话首帧早于本次接入
    let mut got_key = false;
    let mut info_sent = false;
    let cached_key = session.last_key.lock().unwrap().clone();
    if let Some(k) = cached_key {
        // 此刻 SPS/PPS 必已解析（key 在 config 之后到达 pump）
        if send_info(&mut ws, &session).await {
            info_sent = true;
            got_key = true;
            let _ = ws.send(tokio_tungstenite::tungstenite::Message::binary(k)).await;
            tracing::info!("[ws] 已补发缓存关键帧");
        }
    }
    // info（含 avcC description）延迟到首个关键帧前发送：
    // pump 解析 SPS/PPS 晚于本连接建立，过早发送会拿到 None
    #[allow(unused_assignments)]
    let mut info_sent = false;
    async fn send_info(ws: &mut WsStream, session: &SessionHandle) -> bool {
        let avcc = session.avcc_info.lock().unwrap().clone();
        let Some(avcc) = avcc else { return false };
        let mut info = serde_json::json!({
            "width": session.info.width,
            "height": session.info.height,
            "codec": avcc.codec,
            "descriptionB64": avcc.description_b64,
        });
        // 音频元数据在 OpusHead config 包就绪后才下发（作 AudioDecoder description）
        if let Some(a) = audio_info(session) {
            info["audio"] = a;
        }
        let msg = serde_json::json!({ "info": info });
        ws.send(tokio_tungstenite::tungstenite::Message::text(msg.to_string()))
            .await
            .is_ok()
    }

    fn audio_info(session: &SessionHandle) -> Option<serde_json::Value> {
        let cfg = session.audio_cfg.lock().unwrap().clone();
        cfg.map(|c| {
            serde_json::json!({
                "codec": "opus",
                "sampleRate": 48000,
                "channels": 2,
                "descriptionB64": c.description_b64,
            })
        })
    }

    /// 音频 config 包晚于 info 到达时，向已接入客户端补发一条 audioInfo
    async fn ensure_audio_info(
        ws: &mut WsStream,
        session: &SessionHandle,
        sent: &mut bool,
    ) -> bool {
        if *sent {
            return true;
        }
        let Some(a) = audio_info(session) else {
            return false;
        };
        *sent = true;
        ws.send(tokio_tungstenite::tungstenite::Message::text(
            serde_json::json!({ "audioInfo": a }).to_string(),
        ))
        .await
        .is_ok()
    }

    /// 通知客户端设备音频不可用（采集失败/被系统限制，视频不受影响）
    async fn send_audio_disabled(ws: &mut WsStream) {
        let _ = ws
            .send(tokio_tungstenite::tungstenite::Message::text(
                serde_json::json!({ "audioDisabled": true }).to_string(),
            ))
            .await;
    }
    let mut video_rx = session.video_rx.resubscribe();
    // 音频流（None = 会话未开音频；Some 且泵结束后置 None 停止轮询）
    let mut audio_rx = session.audio_rx.as_ref().map(|rx| rx.resubscribe());
    let mut audio_info_sent = false;
    // 接入时音频采集已失败（泵早于本次连接结束）的补报
    if session.audio_enabled
        && session
            .audio_disabled
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        send_audio_disabled(&mut ws).await;
    }
    // 关键帧门控：接入后丢弃 delta 直到首个 key 帧，否则 decoder 报 "A key frame is required"。
    //（SPS/PPS 已在 Rust 端转为 avcC description，经 info JSON 下发，不再进媒体流）
    let mut got_key = false;
    loop {
        tokio::select! {
            chunk = async {
                match audio_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match chunk {
                    Ok(data) => {
                        // 音频帧（kind=3）自包含，无需关键帧门控；先于视频 key 到达也要转发。
                        // config 包（OpusHead description）晚到时先补发 audioInfo
                        if !ensure_audio_info(&mut ws, &session, &mut audio_info_sent).await {
                            continue; // description 未就绪，丢弃直至就绪
                        }
                        if ws.send(tokio_tungstenite::tungstenite::Message::binary(data)).await.is_err() {
                            tracing::info!("[ws] 客户端断开（音频发送失败）");
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => {
                        // 音频泵已结束（设备侧断开），停掉本分支，视频继续
                        audio_rx = None;
                        if session
                            .audio_disabled
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            send_audio_disabled(&mut ws).await;
                        }
                    }
                }
            }
            chunk = video_rx.recv() => {
                match chunk {
                    Ok(data) => {
                        if !got_key {
                            if data.first() == Some(&1) {
                                if !info_sent {
                                    if !send_info(&mut ws, &session).await {
                                        tracing::warn!("[ws] 关键帧就绪但 avcC 仍缺失");
                                        continue;
                                    }
                                    info_sent = true;
                                }
                                got_key = true;
                                tracing::info!("[ws] 首个关键帧转发");
                            } else {
                                continue;
                            }
                        }
                        if ws.send(tokio_tungstenite::tungstenite::Message::binary(data)).await.is_err() {
                            tracing::info!("[ws] 客户端断开（发送失败）");
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => {
                        tracing::info!("[ws] 视频流结束");
                        break;
                    }
                }
            }
            msg = ws.next() => {
                match msg {
                    Some(Ok(m)) => {
                        let stop = m.is_text() && m.into_text().unwrap_or_default() == "stop";
                        if stop {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
    }
    Ok(())
}

// SessionHandle 需要在此模块中可见的公共访问器
impl SessionHandle {
    pub fn is_active(&self) -> bool {
        !matches!(
            &*self.state_rx.borrow(),
            crate::scrcpy::SessionStateInner::Ended(_)
        )
    }

    pub async fn state_reason(&self) -> Option<String> {
        match &*self.state_rx.borrow() {
            crate::scrcpy::SessionStateInner::Ended(r) => Some(r.clone()),
            _ => None,
        }
    }
}
