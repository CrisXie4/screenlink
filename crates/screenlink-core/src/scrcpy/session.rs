//! 投屏会话：拉起 scrcpy server、建立三条 socket、泵视频流与控制通道。

use super::protocol;

use crate::adb::Adb;
use anyhow::{anyhow, Context, Result};
use protocol::{
    parse_device_message, ControlMessage, DeviceMessage, ServerOptions, SCRCPY_VERSION,
};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, watch, Mutex};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, serde::Serialize)]
pub struct VideoInfo {
    pub codec: u32,
    pub width: i32,
    pub height: i32,
}

/// avcC 描述（mp4 风格）与 codec 串，供 WebCodecs configure
#[derive(Debug, Clone, serde::Serialize)]
pub struct AvcInfo {
    pub codec: String,
    #[serde(rename = "descriptionB64")]
    pub description_b64: String,
}

/// 解析 Annex-B 为 NAL 列表（正确处理 3/4 字节起始码）
fn split_annexb(payload: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i + 2 < payload.len() {
        if payload[i] == 0 && payload[i + 1] == 0 && payload[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for k in 0..starts.len() {
        let s = starts[k];
        // 终点 = 下一起始码引导零的起点（3/4 字节形式均适用）
        let e = if k + 1 < starts.len() { starts[k + 1] - 3 } else { payload.len() };
        if e > s {
            nals.push(&payload[s..e]);
        }
    }
    nals
}

/// config 包 -> avcC 盒 + codec 串
pub fn build_avc_info(config_payload: &[u8]) -> Option<AvcInfo> {
    use base64::Engine;
    let nals = split_annexb(config_payload);
    let sps = nals.iter().find(|n| (n[0] & 0x1f) == 7)?;
    let pps = nals.iter().find(|n| (n[0] & 0x1f) == 8)?;
    let mut out = Vec::with_capacity(11 + sps.len() + pps.len());
    out.push(1);
    out.push(sps[1]);
    out.push(sps[2]);
    out.push(sps[3]);
    out.push(0xff); // lengthSizeMinusOne = 3
    out.push(0xe1); // numOfSPS = 1
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // numOfPPS
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    let codec = format!("avc1.{:02X}{:02X}{:02X}", sps[1], sps[2], sps[3]);
    Some(AvcInfo {
        codec,
        description_b64: base64::engine::general_purpose::STANDARD.encode(&out),
    })
}

/// Annex-B -> avcc（每 NAL 前加 4 字节大端长度）
fn annexb_to_avcc(payload: &[u8]) -> Vec<u8> {
    let nals = split_annexb(payload);
    let mut out = Vec::with_capacity(payload.len() + 4 * nals.len());
    for n in nals {
        out.extend_from_slice(&(n.len() as u32).to_be_bytes());
        out.extend_from_slice(n);
    }
    out
}

#[derive(Debug, Clone)]
pub enum SessionStateInner {
    Starting,
    Mirroring,
    Ended(String),
}

/// 上发给渲染端（WS）的视频块：`[u8 kind][i64 pts_be][payload]`
/// kind: 0=CONFIG(SPS/PPS) 1=KEY 2=DELTA 3=AUDIO(opus)
pub const CHUNK_HEADER_LEN: usize = 9;

/// opus 音频流配置（OpusHead config 包，经 WS info 下发作 AudioDecoder description）
#[derive(Debug, Clone)]
pub struct AudioCfg {
    pub description_b64: String,
}

#[derive(Debug, Clone)]
pub enum DeviceEvent {
    Clipboard(String),
}

pub struct SessionHandle {
    pub serial: String,
    pub info: VideoInfo,
    pub video_rx: broadcast::Receiver<Vec<u8>>,
    /// 音频块接收端（kind=3 帧，格式见 CHUNK_HEADER_LEN；audio=false 时为 None）
    pub audio_rx: Option<broadcast::Receiver<Vec<u8>>>,
    /// 音频是否启用（决定 WS info 是否携带 audio 元数据）
    pub audio_enabled: bool,
    /// opus OpusHead config 包（server 发出的首个 CONFIG 帧，作 AudioDecoder description）
    pub audio_cfg: Arc<std::sync::Mutex<Option<AudioCfg>>>,
    /// 设备音频采集失败（server 发了 disable 标记或流中断且从未产出数据帧）
    pub audio_disabled: Arc<std::sync::atomic::AtomicBool>,
    /// SPS/PPS config 包缓存（流启动时只发一次，后接入的渲染端需要补发）
    pub config_cache: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    /// avcC 描述 + codec 串（Rust 端从 config 包解析，经 WS info 下发）
    pub avcc_info: Arc<std::sync::Mutex<Option<AvcInfo>>>,
    /// 最后一个关键帧（avcc 格式块）：静态画面可能长时间无新帧，
    /// 后接入的渲染端必须先拿到 key 才能解码 delta
    pub last_key: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    pub events_rx: broadcast::Receiver<DeviceEvent>,
    pub ctl: Arc<ControlConn>,
    pub state_rx: watch::Receiver<SessionStateInner>,
    stop_tx: watch::Sender<bool>,
    local_port: u16,
}

impl SessionHandle {
    /// 停止会话：发停止信号（监督任务杀掉 adb shell 子进程）并清理 forward。
    pub async fn stop(&self) {
        let _ = self.stop_tx.send(true);
        let serial = self.serial.clone();
        let port = self.local_port;
        let adb = crate::adb::Adb::discover().ok();
        if let Some(adb) = adb {
            let _ = tokio::spawn(async move {
                let _ = adb
                    .run(&[
                        "-s",
                        &serial,
                        "forward",
                        "--remove",
                        &format!("tcp:{port}"),
                    ])
                    .await;
            });
        }
    }
}

pub struct ControlConn {
    writer: Mutex<tokio::net::tcp::OwnedWriteHalf>,
    /// 端到端延迟统计：最近一次注入到当前的时间戳（由 UI 侧另行测量，这里仅保留通道）
    pub connected_at: std::time::Instant,
}

impl ControlConn {
    pub async fn send(&self, msg: ControlMessage) -> Result<()> {
        let bytes = msg.encode();
        let mut w = self.writer.lock().await;
        w.write_all(&bytes).await?;
        w.flush().await?;
        Ok(())
    }
}

/// 定位 scrcpy-server jar：
/// 1) SCREENLINK_SERVER_JAR 环境变量
/// 2) exe 同目录 / assets/
/// 3) 开发期工作区 assets/
pub fn locate_server_jar() -> Result<std::path::PathBuf> {
    if let Ok(p) = std::env::var("SCREENLINK_SERVER_JAR") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for cand in [
            exe.parent().unwrap().join("assets").join(crate::adb::SERVER_JAR_NAME),
            exe.parent().unwrap().join(crate::adb::SERVER_JAR_NAME),
        ] {
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    let dev = std::path::PathBuf::from("assets").join(crate::adb::SERVER_JAR_NAME);
    if dev.is_file() {
        return Ok(std::path::PathBuf::from(".")
            .canonicalize()
            .map(|p| p.join(&dev))
            .unwrap_or(dev));
    }
    Err(anyhow!(
        "未找到 {}（可通过 SCREENLINK_SERVER_JAR 指定路径）",
        crate::adb::SERVER_JAR_NAME
    ))
}

/// 启动投屏会话（video + control；opts.audio=true 时加 audio 通道，opus 48kHz）。
///
/// audio=true 时 socket 连接顺序必须为 video -> audio -> control（server 按此顺序
/// accept）；audio 帧格式与视频共用 Streamer.writeFrameMeta（12B 头），首个 CONFIG
/// 帧为 OpusHead。设备采集失败时 server 发 4B 全零标记并继续仅视频。
pub async fn start_session(
    adb: &Adb,
    serial: &str,
    opts: ServerOptions,
    jar_path: &std::path::Path,
) -> Result<SessionHandle> {
    // scid 必须每次唯一：僵尸 server 可能仍占用旧 abstract socket 名，
    // 撞名会导致 forward 连接到死监听（EOF）。这里强制重新生成（31bit 非零）。
    let opts = ServerOptions {
        scid: ((std::process::id() as u64 ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0))
            & 0x7fff_ffff)
            .max(1) as u32,
        ..opts
    };
    // 1. push server jar（大小一致则跳过，避免重复推 70KB）
    let remote_jar = format!("/data/local/tmp/{}", crate::adb::SERVER_JAR_NAME);
    let local_size = std::fs::metadata(jar_path)?.len();
    let need_push = adb
        .shell(
            serial,
            &format!("stat -c %s {remote_jar} 2>/dev/null || echo -1"),
        )
        .await
        .map(|s| s.trim() != local_size.to_string())
        .unwrap_or(true);
    if need_push {
        adb.push(
            serial,
            &[jar_path.to_path_buf()],
            "/data/local/tmp",
            crate::adb::CancelToken::new(),
            |_, _, _| {},
        )
        .await
        .context("推送 scrcpy server 到设备失败")?;
    }

    // 2. 本地空闲端口 + adb forward 隧道（TCP 通道设计核心：
    //    设备端 LocalServerSocket `scrcpy_<scid>`，PC 经 forward 三次连接，顺序 video->control）
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let local_port = listener.local_addr()?.port();
    drop(listener);
    let scid = opts.scid;
    let sock_name = format!("scrcpy_{scid:08x}");
    adb.forward(serial, &format!("tcp:{local_port}"), &format!("localabstract:{sock_name}"))
        .await
        .context("建立 adb forward 隧道失败")?;

    // 3. 启动设备端 server
    let mut args: Vec<String> = vec![
        "-s".into(),
        serial.to_string(),
        "shell".into(),
        format!("CLASSPATH={remote_jar} app_process / com.genymobile.scrcpy.Server"),
    ];
    args.extend(opts.to_args());
    let mut cmd = tokio::process::Command::new(&adb.path);
    cmd.args(&args);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // 关键：stdin 必须保持打开。adb shell 在 stdin EOF 时会立刻终止设备端会话；
    // 用一根永不写入的管道托住 stdin，server 生命周期即与会话一致。
    cmd.stdin(std::process::Stdio::piped());
    crate::util::no_window(&mut cmd);
    let mut child = cmd.spawn().context("启动 scrcpy server 进程失败")?;
    // 持有 stdin 写端：落盘到监督任务，会话结束才关闭
    let stdin = child.stdin.take();
    let stderr = child.stderr.take();
    let debug = std::env::var("SCREENLINK_DEBUG").is_ok();
    if debug {
        eprintln!("[sl-debug] adb args: {args:?}");
    }
    // 调试模式：server stderr 直接透传，便于排查启动失败
    let stderr = if debug {
        stderr.map(|mut s| {
            tokio::spawn(async move {
                let mut tmp = [0u8; 2048];
                loop {
                    match s.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            eprint!("{}", String::from_utf8_lossy(&tmp[..n]));
                        }
                    }
                }
            });
        });
        None
    } else {
        stderr
    };

    // 4. 依次建立三条 TCP 连接。
    // 竞态处理：server 进程冷启动需数百毫秒才开始监听，adbd 在设备侧连接被拒时
    // 会立刻关闭我们的 TCP（表现为 EOF）。与 scrcpy 官方客户端一致：循环重试。
    let dbg = |step: &str| {
        if std::env::var("SCREENLINK_DEBUG").is_ok() {
            eprintln!("[sl-debug] {step}");
        }
    };
    let connect_with_retry = |port: u16, first_socket: bool| async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            match async {
                let mut s = TcpStream::connect(("127.0.0.1", port)).await?;
                s.set_nodelay(true)?; // 低延迟关键：禁用 Nagle
                if first_socket {
                    // dummy byte 只在第一个 accept 的 socket（video）上发一次
                    let mut d = [0u8; 1];
                    tokio::time::timeout(
                        std::time::Duration::from_millis(1500),
                        s.read_exact(&mut d),
                    )
                    .await
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "dummy 超时"))??;
                }
                Ok::<TcpStream, std::io::Error>(s)
            }
            .await
            {
                Ok(s) => return Ok(s),
                Err(_) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(anyhow!("8s 内未能建立连接（server 未启动？）"));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    };

    dbg("connecting video socket (retry loop)");
    let mut video_rd = connect_with_retry(local_port, true)
        .await
        .map_err(|e| anyhow!("连接 video socket 失败: {e}"))?;
    dbg("video connected + dummy ok");
    // audio socket：scrcpy server 按 video -> audio -> control 顺序 accept，
    // 开启音频时必须在 control 之前连接，否则 control 会错位到 audio 监听。
    let audio_enabled = opts.audio;
    let mut audio_rd = if audio_enabled {
        Some(
            connect_with_retry(local_port, false)
                .await
                .map_err(|e| anyhow!("连接 audio socket 失败: {e}"))?,
        )
    } else {
        None
    };
    dbg("connecting control socket");
    let mut control = connect_with_retry(local_port, false)
        .await
        .map_err(|e| anyhow!("连接 control socket 失败: {e}"))?;
    dbg("control connected");
    // 注意：scrcpy 的 send_dummy_byte 只在第一个 accept 的 socket（video）上发一次，
    // audio/control socket 不写 dummy —— 不能在这里读，否则双方死锁。

    dbg("reading device meta");
    // 5. video socket：64B 设备名 + 12B 编解码头
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        video_rd.read_exact(&mut vec![0u8; 64]),
    )
    .await
    .map_err(|_| anyhow!("读取 device meta 超时"))??;
    let meta = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::util::read_exact(&mut video_rd, 12),
    )
    .await
    .map_err(|_| anyhow!("读取 video header 超时"))??;
    dbg("meta ok, reading video header");
    let codec = crate::util::parse_u32_be(&meta[0..4]);
    let width = i32::from_be_bytes([meta[4], meta[5], meta[6], meta[7]]);
    let height = i32::from_be_bytes([meta[8], meta[9], meta[10], meta[11]]);

    // audio socket 握手：send_codec_meta=true 时开头 4B codec id（"opus"），
    // 其后帧格式与视频一致（[u64 pts|flags][u32 len]，见下方音频泵注释）
    if let Some(rd) = audio_rd.as_mut() {
        let codec_meta = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            crate::util::read_exact(rd, 4),
        )
        .await
        .map_err(|_| anyhow!("读取 audio codec meta 超时"))??;
        let codec_id = crate::util::parse_u32_be(&codec_meta);
        if codec_id != protocol::CODEC_OPUS {
            anyhow::bail!("设备音频编解码非 opus（{codec_id:#010x}），请关闭音频转发");
        }
        dbg("audio codec meta ok (opus)");
    }

    let (video_tx, video_rx) = broadcast::channel::<Vec<u8>>(128);
    let (audio_tx, audio_rx) = broadcast::channel::<Vec<u8>>(256);
    let (events_tx, events_rx) = broadcast::channel::<DeviceEvent>(32);
    let (state_tx, state_rx) = watch::channel(SessionStateInner::Mirroring);
    let (stop_tx, stop_rx) = watch::channel(false);

    // 6. 视频泵：帧头(u64 pts|flags + u32 len) -> 统一 9B 头广播给 WS 桥
    let config_cache: Arc<std::sync::Mutex<Option<Vec<u8>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let config_cache2 = config_cache.clone();
    let avcc_info: Arc<std::sync::Mutex<Option<AvcInfo>>> =
        Arc::new(std::sync::Mutex::new(None));
    let avcc_info2 = avcc_info.clone();
    let last_key: Arc<std::sync::Mutex<Option<Vec<u8>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let last_key2 = last_key.clone();
    let video_pump: JoinHandle<()> = tokio::spawn(async move {
        let config_cache = config_cache2;
        let avcc_info = avcc_info2;
        let last_key = last_key2;
        let mut rd = video_rd;
        loop {
            let hdr = match crate::util::read_exact(&mut rd, 12).await {
                Ok(h) => h,
                Err(_) => break,
            };
            let pts_and_flags = crate::util::parse_i64_be(&hdr[0..8]) as u64;
            let len = crate::util::parse_u32_be(&hdr[8..12]) as usize;
            let payload = match crate::util::read_exact(&mut rd, len).await {
                Ok(p) => p,
                Err(_) => break,
            };
            let kind: u8 = if pts_and_flags & protocol::PACKET_FLAG_CONFIG != 0 {
                0
            } else if pts_and_flags & protocol::PACKET_FLAG_KEY_FRAME != 0 {
                1
            } else {
                2
            };
            // 剥离 CONFIG/KEYFRAME 标志位，得到纯 pts（µs）——否则 key 帧的
            // timestamp 高达 4.6e18，前端 Number 后超出 WebCodecs long long 范围
            let pts = if kind == 0 {
                0
            } else {
                (pts_and_flags & !(protocol::PACKET_FLAG_CONFIG | protocol::PACKET_FLAG_KEY_FRAME)) as i64
            };
            // WebCodecs(WebView2) 只接受 avcC description + 4 字节长度前缀块：
            // config 包在 Rust 侧解析出 avcC/codec（经 WS info 下发），媒体帧转 avcc
            if kind == 0 {
                if let Some(info) = build_avc_info(&payload) {
                    *avcc_info.lock().unwrap() = Some(info);
                }
                continue;
            }
            let avcc_payload = annexb_to_avcc(&payload);
            let mut chunk = Vec::with_capacity(CHUNK_HEADER_LEN + avcc_payload.len());
            chunk.push(kind);
            chunk.extend_from_slice(&(pts as i64).to_be_bytes());
            chunk.extend_from_slice(&avcc_payload);
            if kind == 1 {
                *last_key.lock().unwrap() = Some(chunk.clone());
            }
            if video_tx.send(chunk).is_err() {
                break; // 无订阅者（UI 已关闭）
            }
        }
    });

    // 6.5 音频泵：帧头与视频完全一致（[u64 pts|flags][u32 len]，Streamer.writeFrameMeta
    // 音视频共用，12 字节——不是 4 字节长度前缀）。首帧为 CONFIG（OpusHead，
    // scrcpy fixOpusConfigPacket 已裁好），缓存后经 WS info 下发作 AudioDecoder
    // description，不进数据流。设备采集失败时 server 写 4B 全零 disable 标记。
    let audio_cfg: Arc<std::sync::Mutex<Option<AudioCfg>>> =
        Arc::new(std::sync::Mutex::new(None));
    let audio_cfg2 = audio_cfg.clone();
    let audio_disabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let audio_disabled2 = audio_disabled.clone();
    let audio_pump: Option<JoinHandle<()>> = audio_rd.map(|mut rd| {
        tokio::spawn(async move {
            let audio_cfg = audio_cfg2;
            let audio_disabled = audio_disabled2;
            let mut got_data = false;
            loop {
                let hdr = match crate::util::read_exact(&mut rd, 12).await {
                    Ok(h) => h,
                    Err(_) => break,
                };
                let pts_and_flags = crate::util::parse_i64_be(&hdr[0..8]) as u64;
                let len = crate::util::parse_u32_be(&hdr[8..12]) as usize;
                // disable 标记 / 异常长度：opus 包正常 <1KB，上限 4MiB 防御
                if len == 0 || len > (1 << 22) {
                    break;
                }
                let payload = match crate::util::read_exact(&mut rd, len).await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                if pts_and_flags & protocol::PACKET_FLAG_CONFIG != 0 {
                    use base64::Engine as _;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&payload);
                    *audio_cfg.lock().unwrap() = Some(AudioCfg { description_b64: b64 });
                    continue;
                }
                got_data = true;
                let pts = (pts_and_flags
                    & !(protocol::PACKET_FLAG_CONFIG | protocol::PACKET_FLAG_KEY_FRAME))
                    as i64;
                let mut chunk = Vec::with_capacity(CHUNK_HEADER_LEN + payload.len());
                chunk.push(3u8);
                chunk.extend_from_slice(&pts.to_be_bytes());
                chunk.extend_from_slice(&payload);
                if audio_tx.send(chunk).is_err() {
                    break; // 无订阅者（UI 已关闭）
                }
            }
            if !got_data {
                audio_disabled.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })
    });

    // 7. 控制泵：解析设备消息（剪贴板等）
    let (ctl_rd, ctl_wr) = control.into_split();
    let ctl = Arc::new(ControlConn {
        writer: Mutex::new(ctl_wr),
        connected_at: std::time::Instant::now(),
    });
    let ctl_pump: JoinHandle<()> = tokio::spawn(async move {
        let mut rd = ctl_rd;
        let mut buf: Vec<u8> = Vec::with_capacity(8192);
        let mut tmp = [0u8; 4096];
        loop {
            match rd.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    while let Some((msg, consumed)) = parse_device_message(&buf) {
                        buf.drain(..consumed);
                        match msg {
                            DeviceMessage::Clipboard(text) => {
                                let _ = events_tx.send(DeviceEvent::Clipboard(text));
                            }
                            _ => {}
                        }
                    }
                    if buf.len() > (1 << 20) {
                        buf.clear(); // 防御：非法流
                    }
                }
            }
        }
    });

    // 8. 生命周期监督：adb shell 退出 / stop 信号 -> Ended
    let serial_owned = serial.to_string();
    let state_tx2 = state_tx.clone();
    let supervisor: JoinHandle<()> = tokio::spawn(async move {
        let mut stop_rx = stop_rx;
        let _keep_stdin = stdin; // 保持 server stdin 打开
        let stderr_handle = stderr.map(|mut s| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                while let Ok(n) = s.read(&mut tmp).await {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.len() > 64 * 1024 {
                        break;
                    }
                }
                String::from_utf8_lossy(&buf).into_owned()
            })
        });

        let exit_status = tokio::select! {
            st = child.wait() => st.ok().and_then(|s| s.code()).unwrap_or(-1),
            _ = stop_rx.changed() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                -2
            }
        };
        let tail = match stderr_handle {
            Some(h) => h.await.unwrap_or_default(),
            None => String::new(),
        };
        let reason = if exit_status == -2 {
            "stopped".to_string()
        } else if tail.contains("doesn't match") {
            format!("server 版本不匹配（期望 {SCRCPY_VERSION}）")
        } else if !tail.is_empty() {
            tail.lines().last().unwrap_or("").to_string()
        } else {
            format!("server 退出 (code {exit_status})")
        };
        let _ = state_tx2.send(SessionStateInner::Ended(reason));
    });

    drop(state_tx); // 保留 receiver 侧即可感知结束

    // 泵任务 JoinHandle 丢弃即后台运行（与 video_pump 同模式），仅抑制未使用告警
    let _ = (video_pump, audio_pump, ctl_pump, supervisor);

    Ok(SessionHandle {
        serial: serial_owned,
        info: VideoInfo { codec, width, height },
        video_rx,
        audio_rx: if audio_enabled { Some(audio_rx) } else { None },
        audio_enabled,
        audio_cfg,
        audio_disabled,
        config_cache,
        avcc_info,
        last_key,
        events_rx,
        ctl,
        state_rx,
        stop_tx,
        local_port,
    })
}
