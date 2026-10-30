//! ScreenLink 命令行调试工具（也是无 GUI 的集成测试入口）。
//!
//! 用法示例：
//!   screenlink-cli devices
//!   screenlink-cli mdns
//!   screenlink-cli mirror --serial <S> --frames 120     # 收帧统计
//!   screenlink-cli screenshot --serial <S> -o shot.png  # adb screencap
//!   screenlink-cli tap --serial <S> --x 540 --y 1200
//!   screenlink-cli text --serial <S> "hello"
//!   screenlink-cli key --serial <S> back|home|enter
//!   screenlink-cli push --serial <S> <local> <remote_dir>
//!   screenlink-cli pull --serial <S> <remote> <local_dir>
//!   screenlink-cli page --serial <S>                    # uiautomator dump 解析
//!   screenlink-cli wifi-scan                            # mDNS + 子网 5555
//!   screenlink-cli connect <ip:port> / pair <ip:port> <code>

use screenlink_core::{adb::Adb, discovery::Discovery};

fn arg_of(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("错误: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // 摘除 --serial <value>，保证位置参数索引稳定
    let serial = {
        let mut out = None;
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--serial" && i + 1 < args.len() {
                out = Some(args[i + 1].clone());
                args.drain(i..=i + 1);
            } else {
                i += 1;
            }
        }
        out
    };
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let adb = Adb::discover().expect("adb 不可用（设置 SCREENLINK_ADB）");

    let res = match cmd {
        "devices" => {
            for d in adb.devices().await.unwrap_or_default() {
                println!(
                    "{:<28} {:<12} {:<8} {}",
                    d.serial,
                    format!("{:?}", d.transport),
                    format!("{:?}", d.state),
                    d.model
                );
            }
            Ok(())
        }
        "props" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let ip = adb.wifi_ip(&s).await;
            let battery = adb.battery(&s).await;
            println!("wifi_ip={ip:?} battery={battery:?}");
            Ok(())
        }
        "mdns" => {
            for svc in adb.mdns_services().await.unwrap_or_default() {
                println!("{}  {}  {}", svc.name, svc.addr, svc.service_type);
            }
            Ok(())
        }
        "wifi-scan" => {
            let r = Discovery::scan(&adb).await;
            println!("mDNS:");
            for s in &r.mdns {
                println!("  {} {} {}", s.name, s.addr, s.service_type);
            }
            println!("UDP agent: {:?}", r.udp);
            println!("5555 扫描: {:?}", r.scanned_5555);
            Ok(())
        }
        "connect" => {
            let addr = args.get(1).cloned().expect("用法: connect <ip:port>");
            println!("{}", adb.connect(&addr).await?);
            Ok(())
        }
        "pair" => {
            let addr = args.get(1).cloned().expect("用法: pair <ip:port> <code>");
            let code = args.get(2).cloned().expect("用法: pair <ip:port> <code>");
            println!("{}", adb.pair(&addr, &code).await?);
            Ok(())
        }
        "screenshot" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let png = adb.screencap_png(&s).await?;
            let out = arg_of(&args, "-o").unwrap_or_else(|| "shot.png".into());
            std::fs::write(&out, &png)?;
            println!("{} bytes -> {}", png.len(), out);
            Ok(())
        }
        "mirror" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let frames: usize = arg_of(&args, "--frames").and_then(|f| f.parse().ok()).unwrap_or(120);
            let audio_on = args.iter().any(|a| a == "--audio");
            let jar = screenlink_core::scrcpy::session::locate_server_jar()
                .expect("scrcpy server jar 未找到");
            let t0 = std::time::Instant::now();
            let session = screenlink_core::scrcpy::start_session(
                &adb,
                &s,
                screenlink_core::scrcpy::protocol::ServerOptions { audio: audio_on, ..Default::default() },
                &jar,
            )
            .await?;
            println!(
                "session started in {}ms: {}x{} codec={:08x} audio={}",
                t0.elapsed().as_millis(),
                session.info.width,
                session.info.height,
                session.info.codec,
                session.audio_enabled,
            );
            {
                let info = session.avcc_info.lock().unwrap().clone();
                use base64::Engine;
                if let Some(i) = info {
                    let raw = base64::engine::general_purpose::STANDARD
                        .decode(&i.description_b64)
                        .unwrap_or_default();
                    println!(
                        "avcC codec={} len={} hex={}",
                        i.codec,
                        raw.len(),
                        raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
                    );
                } else {
                    println!("avcC: <none>");
                }
            }
            let mut rx = session.video_rx.resubscribe();
            let mut audio_rx = session.audio_rx.as_ref().map(|r| r.resubscribe());
            let mut n = 0usize;
            let mut key_dumped = false;
            let mut bytes = 0u64;
            let mut a_pkts = 0usize;
            let mut a_bytes = 0u64;
            let first_key = std::time::Instant::now();
            let mut first_key_ms = 0u128;
            while n < frames {
                tokio::select! {
                    chunk = async {
                        match audio_rx.as_mut() {
                            Some(r) => r.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        match chunk {
                            Ok(c) => {
                                if a_pkts == 0 {
                                    println!("first audio chunk: kind={} len={} payload_head={}",
                                        c[0], c.len(), c.iter().skip(9).take(8).map(|b| format!("{b:02x}")).collect::<String>());
                                }
                                a_pkts += 1;
                                a_bytes += (c.len() - 9) as u64;
                            }
                            Err(e) => { eprintln!("audio recv err: {e}"); break; }
                        }
                    }
                    chunk = rx.recv() => {
                        match chunk {
                    Ok(chunk) => {
                        if chunk[0] == 1 && first_key_ms == 0 {
                            first_key_ms = first_key.elapsed().as_millis();
                        }
                        if chunk[0] == 1 && !key_dumped {
                            key_dumped = true;
                            let hex: String = chunk.iter().take(25).map(|b| format!("{b:02x}")).collect();
                            println!("first key chunk (avcc) head: {} total={}", hex, chunk.len());
                        }
                        n += 1;
                        bytes += chunk.len() as u64;
                    }
                    Err(e) => {
                        eprintln!("recv err: {e}");
                        break;
                    }
                }
                    }
                }
            }
            let el = first_key.elapsed().as_secs_f64();
            println!(
                "received {n} chunks, {:.2} MB, avg {:.0} KB/chunk, first_key={}ms, {:.1} fps; audio packets={a_pkts} bytes={a_bytes}",
                bytes as f64 / 1e6,
                bytes as f64 / 1e3 / n.max(1) as f64,
                first_key_ms,
                n as f64 / el
            );
            session.stop().await;
            Ok(())
        }
        "tap" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let x: i32 = arg_of(&args, "--x").and_then(|v| v.parse().ok()).expect("--x");
            let y: i32 = arg_of(&args, "--y").and_then(|v| v.parse().ok()).expect("--y");
            let w: u16 = arg_of(&args, "--w").and_then(|v| v.parse().ok()).unwrap_or(1080);
            let h: u16 = arg_of(&args, "--h").and_then(|v| v.parse().ok()).unwrap_or(2400);
            // 快路径：adb shell input（无会话时）；协议正确性由 mirror 会话测试覆盖
            let out = adb.run(&["-s", &s, "shell", "input", "tap", &x.to_string(), &y.to_string()]).await?;
            let _ = (w, h);
            println!("tap ok ({} bytes err)", out.stderr.len());
            Ok(())
        }
        "text" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let text = args.get(1).cloned().unwrap_or_default();
            adb.run(&["-s", &s, "shell", "input", "text", &text]).await?;
            println!("text ok");
            Ok(())
        }
        "key" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let key = args.get(1).cloned().unwrap_or_default();
            let code = match key.as_str() {
                "back" => "4",
                "home" => "3",
                "enter" => "66",
                "esc" => "111",
                "vol_up" => "24",
                "vol_down" => "25",
                other => other,
            };
            adb.run(&["-s", &s, "shell", "input", "keyevent", code]).await?;
            println!("key ok");
            Ok(())
        }
        "push" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let local = std::path::PathBuf::from(args.get(1).cloned().expect("local path"));
            let remote = args.get(2).cloned().expect("remote dir");
            let t0 = std::time::Instant::now();
            adb.push(&s, &[local], &remote, screenlink_core::adb::CancelToken::new(), |_c, _t, p| {
                if p % 20 == 0 {
                    print!("\r{p}%");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
            })
            .await?;
            println!("\npush done in {:?}", t0.elapsed());
            Ok(())
        }
        "pull" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let remote = args.get(1).cloned().expect("remote path");
            let local = std::path::PathBuf::from(args.get(2).cloned().expect("local dir"));
            let t0 = std::time::Instant::now();
            adb.pull(&s, &remote, &local, screenlink_core::adb::CancelToken::new(), |_c, _t, p| {
                if p % 20 == 0 {
                    print!("\r{p}%");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
            })
            .await?;
            println!("\npull done in {:?}", t0.elapsed());
            Ok(())
        }
        "page" => {
            let s = match serial.clone() { Some(s) => s, None => first_serial(&adb).await };
            let xml = adb.ui_xml(&s).await?;
            let state = parse_page(&xml);
            println!("elements: {}", state.len());
            for (i, e) in state.iter().enumerate() {
                println!(
                    "[{:>2}] {:<24} text={:<20} bounds={} center=({},{})",
                    i, e.class, e.text, e.bounds, e.cx, e.cy
                );
            }
            Ok(())
        }
        "agent-send" => {
            // 用法: agent-send <ip:port> <token> <file> [name]
            let addr = args.get(1).cloned().expect("ip:port");
            let token = args.get(2).cloned().expect("token");
            let file = std::path::PathBuf::from(args.get(3).cloned().expect("file"));
            let name = args
                .get(4)
                .cloned()
                .unwrap_or_else(|| file.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
            let target = screenlink_core::agent_link::AgentTarget { addr, token };
            let t0 = std::time::Instant::now();
            let mut link = screenlink_core::agent_link::AgentLink::connect(&target).await?;
            println!("agent connected in {:?}", t0.elapsed());
            let size = std::fs::metadata(&file)?.len();
            let t1 = std::time::Instant::now();
            link.send_file(&file, &name, &screenlink_core::adb::CancelToken::new(), |c, total| {
                print!("\r{}/{} ({:.0}%)", c, total, c as f64 / total.max(1) as f64 * 100.0);
                use std::io::Write;
                let _ = std::io::stdout().flush();
            })
            .await?;
            let el = t1.elapsed().as_secs_f64();
            println!("\nsend done: {} bytes in {:.2}s = {:.1} MB/s", size, el, size as f64 / 1e6 / el);
            Ok(())
        }
        "help" | _ => {
            println!("{}", HELP);
            Ok(())
        }
    };
    res
}

async fn first_serial(adb: &Adb) -> String {
    adb.devices()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|x| x.state == screenlink_core::device::DeviceState::Online)
        .map(|d| d.serial)
        .expect("无在线设备，请用 --serial 指定")
}

#[derive(Debug, Clone)]
struct PageElement {
    class: String,
    text: String,
    bounds: String,
    cx: i32,
    cy: i32,
}

fn parse_page(xml: &str) -> Vec<PageElement> {
    let mut out = Vec::new();
    if let Ok(doc) = roxmltree::Document::parse(xml) {
        for node in doc.descendants() {
            if node.tag_name().name() != "node" {
                continue;
            }
            let a = |k: &str| node.attribute(k).unwrap_or("");
            let bounds = a("bounds").to_string();
            let (cx, cy) = center_of(&bounds);
            if a("clickable") == "true" || !a("text").is_empty() || !a("content-desc").is_empty() {
                out.push(PageElement {
                    class: a("class").to_string(),
                    text: if a("text").is_empty() { a("content-desc").to_string() } else { a("text").to_string() },
                    bounds,
                    cx,
                    cy,
                });
            }
        }
    }
    out
}

fn center_of(bounds: &str) -> (i32, i32) {
    // "[x1,y1][x2,y2]"
    let nums: Vec<i32> = bounds
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() == 4 {
        ((nums[0] + nums[2]) / 2, (nums[1] + nums[3]) / 2)
    } else {
        (0, 0)
    }
}

const HELP: &str = r#"ScreenLink CLI
  devices | props | mdns | wifi-scan | connect | pair
  screenshot | mirror | tap | text | key | push | pull | page
  通用参数: --serial <SERIAL>
"#;
