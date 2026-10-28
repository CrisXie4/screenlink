//! 伴侣 App 协议 Rust 客户端真机测试：需要一台运行伴侣 App 服务（或 shell agent）的手机。
//! 无设备/连接失败时跳过（不打 fail）。
//!
//! verify：用已保存 token 走 AUTH（若 PC 端从未配对则跳过）；
//! pair：触发 NEED_PAIR → 从设备通知栏抓 6 位码 → PAIR → 校验 PAIR_OK 与持久化。

use screenlink_core::agent_client;

const PHONE_IP: &str = "192.168.10.20";

#[tokio::test]
async fn verify_paired_token() {
    let phones = agent_client::load_paired();
    let Some(phone) = phones.first() else {
        eprintln!("跳过：PC 端无已配对手机");
        return;
    };
    match agent_client::verify(PHONE_IP, &phone.token).await {
        Ok(name) => println!("verify ok: {name} (token {})", phone.token_id),
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("连接手机失败") || msg.contains("拒绝") || msg.contains("timed out") {
                eprintln!("跳过：手机不可达（{msg}）");
            } else {
                panic!("verify 失败: {msg}");
            }
        }
    }
}

#[tokio::test]
async fn pair_flow_via_notification_code() {
    // 幂等：若已配对则直接验证并返回，不重复烧配对码
    if !agent_client::load_paired().is_empty() {
        eprintln!("跳过：已有配对（verify 用例覆盖）");
        return;
    }
    let session = match agent_client::pair_begin(PHONE_IP, 27188).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("跳过：手机不可达（{e:#}）");
            return;
        }
    };
    // 从设备通知栏抓码（AgentService 把配对码写进常驻通知）
    let mut code = None;
    for _ in 0..15 {
        let dump = std::process::Command::new("adb")
            .args(["-s", "10AF3P06Y3001JX", "shell", "dumpsys", "notification", "--noredact"])
            .output()
            .expect("adb");
        let text = String::from_utf8_lossy(&dump.stdout);
        if let Some(pos) = text.find("配对码 ") {
            code = Some(text[pos + 12..pos + 18].to_string());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let Some(code) = code else {
        session.cancel();
        eprintln!("跳过：未在通知栏找到配对码");
        return;
    };
    assert_eq!(code.len(), 6, "配对码应为 6 位");
    let phone = session.submit_code(&code).await.expect("submit_code");
    println!("paired: {} ({})", phone.name, phone.token_id);
    let saved = agent_client::load_paired();
    assert!(saved.iter().any(|p| p.token_id == phone.token_id), "token 未持久化");
    let name = agent_client::verify(PHONE_IP, &phone.token).await.expect("verify after pair");
    println!("verify after pair ok: {name}");
}
