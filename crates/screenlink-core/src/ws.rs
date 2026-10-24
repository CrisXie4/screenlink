//! WebSocket 流桥：把投屏会话的视频块推给 webview 渲染端（WebCodecs 解码）。
//!
//! 路由：`ws://127.0.0.1:<port>/mirror/{serial}`
//! 消息即 scrcpy/session.rs 定义的 9B 头 + H.264 负载（Annex-B）。
//! 选 WebSocket 而非 Tauri IPC 的原因：连续二进制流无 JSON/拷贝开销，本地回环 <1ms。

use crate::scrcpy::SessionHandle;
use anyhow::Result;
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
    let mut ws = tokio_tungstenite::accept_async(stream).await?;
    // 客户端首条文本消息 = "mirror:<serial>"（避免依赖 HTTP upgrade path 的解析差异）
    let serial = match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
        Ok(Some(Ok(msg))) => {
            let text = msg.into_text().unwrap_or_default();
            text.strip_prefix("mirror:").unwrap_or("").to_string()
        }
        _ => return Ok(()),
    };
    if serial.is_empty() {
        return Ok(());
    }
    let session = sessions.lock().unwrap().get(&serial).cloned();
    let Some(session) = session else {
        let _ = ws.send(tokio_tungstenite::tungstenite::Message::text("error:no-session")).await;
        return Ok(());
    };
    let _ = ws
        .send(tokio_tungstenite::tungstenite::Message::text(
            serde_json::to_string(&serde_json::json!({
                "info": { "width": session.info.width, "height": session.info.height, "codec": format!("{:08x}", session.info.codec) }
            }))
            .unwrap(),
        ))
        .await;
    let mut video_rx = session.video_rx.resubscribe();
    // SPS/PPS config 补发 + 关键帧门控：
    // config 包在流启动时只出现一次，pump 写入缓存可能晚于本连接建立——
    // 因此在发送第一个媒体帧前检查缓存，有则先补发 config。
    // 门控：丢弃 delta 直到首个 key 帧，否则 decoder 报 "A key frame is required"。
    let mut got_key = false;
    let mut config_sent = false;
    loop {
        tokio::select! {
            chunk = video_rx.recv() => {
                match chunk {
                    Ok(data) => {
                        if !config_sent {
                            let cached = session.config_cache.lock().unwrap().clone();
                            if let Some(cfg) = cached {
                                let _ = ws.send(tokio_tungstenite::tungstenite::Message::binary(cfg)).await;
                                config_sent = true;
                            }
                        }
                        if !got_key {
                            if data.first() == Some(&1) {
                                got_key = true;
                            } else {
                                continue;
                            }
                        }
                        if ws.send(tokio_tungstenite::tungstenite::Message::binary(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
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
