//! 真机集成测试：需要一台已授权的安卓设备（USB / adb-wifi 均可）。
//! 无设备时全部测试自动跳过（CI 无设备环境不红）。
//!
//! 覆盖 transfer.rs 新 push 通道的核心可靠性语义：
//! - 逐文件原子落盘（成功后远端无 .sl-part-* 残留）
//! - 子目录结构还原（远端 mkdir -p）
//! - Overwrite / Rename / Skip 三种冲突策略
//! - 内容一致性（本地 certutil md5 vs 设备端 md5sum）

use screenlink_core::adb::Adb;
use screenlink_core::transfer::{ConflictPolicy, JobKind, JobState, TransferManager};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const REMOTE_DIR: &str = "/sdcard/Download/ScreenLink-test";

fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

async fn online_serial() -> Option<String> {
    let adb = Adb::discover().ok()?;
    adb.devices()
        .await
        .ok()?
        .into_iter()
        .find(|d| d.serial.contains('_') == false && {
            use screenlink_core::device::DeviceState;
            d.state == DeviceState::Online
        })
        .map(|d| d.serial)
}

async fn wait_job(mgr: &TransferManager, id: u64) -> screenlink_core::transfer::JobSnapshot {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last_print = 0u64;
    loop {
        if let Some(s) = mgr.snapshots().into_iter().find(|s| s.id == id) {
            let elapsed = s.copied_bytes;
            if elapsed - last_print > 2 * 1024 * 1024 {
                last_print = elapsed;
                eprintln!("  进度: {} bytes / {}", s.copied_bytes, s.total_bytes);
            }
            if !matches!(s.state, JobState::Queued | JobState::Running) {
                return s;
            }
            assert!(Instant::now() < deadline, "任务 {id} 超时未完成，最终状态 {:?} 已传 {}B 错误 {:?}", s.state, s.copied_bytes, s.error);
        } else {
            assert!(Instant::now() < deadline, "任务 {id} 不存在");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn local_md5(path: &std::path::Path) -> String {
    let out = std::process::Command::new("certutil")
        .args(["-hashfile", &path.to_string_lossy(), "MD5"])
        .output()
        .expect("certutil");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .find(|l| l.len() == 32 && l.chars().all(|c| c.is_ascii_hexdigit()))
        .expect("certutil 输出中未找到 md5")
        .to_ascii_lowercase()
}

async fn remote_md5(adb: &Adb, serial: &str, path: &str) -> String {
    let out = adb.shell(serial, &format!("md5sum {}", shq(path))).await.unwrap();
    out.split_whitespace().next().unwrap().to_ascii_lowercase()
}

/// 确定性伪随机内容（LCG），避免全零数据掩盖问题
fn fill(buf: &mut Vec<u8>, seed: u64, len: usize) {
    let mut x = seed;
    buf.clear();
    for _ in 0..len {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        buf.push((x >> 33) as u8);
    }
}

#[tokio::test]
async fn push_atomic_subdir_and_conflict_policies() {
    let Some(serial) = online_serial().await else {
        eprintln!("跳过：无在线设备");
        return;
    };
    let adb = Adb::discover().unwrap();
    let mgr = TransferManager::new(adb.clone());

    // 干净起点（上次运行失败也可能留下残留）
    adb.shell(&serial, &format!("rm -rf {}", shq(REMOTE_DIR))).await.unwrap();
    adb.shell(&serial, &format!("mkdir -p {}", shq(REMOTE_DIR))).await.unwrap();

    // 本地夹具：顶层中文+空格名、子目录文件
    let local = std::env::temp_dir().join(format!("sl-transfer-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(local.join("sub/nested")).unwrap();
    let mk = |name: &str, seed: u64, len: usize| {
        let mut data = Vec::new();
        fill(&mut data, seed, len);
        let p = local.join(name);
        std::fs::write(&p, &data).unwrap();
        p
    };
    let f_a = mk("a.txt", 1, 64 * 1024);
    let f_zh = mk("中文名 文件.mp4", 2, 256 * 1024);
    let f_b = mk("sub/nested/b.bin", 3, 128 * 1024);
    let files = vec![f_a.clone(), f_zh.clone(), f_b.clone()];
    let rels = vec!["a.txt".into(), "中文名 文件.mp4".into(), "sub/nested/b.bin".into()];
    let remote = |name: &str| format!("{REMOTE_DIR}/{name}");

    // ---- 1. Overwrite 首推：结构 + 内容一致性 + 无 .part 残留 ----
    let id = mgr.enqueue(JobKind::Push, &serial, files.clone(), rels.clone(), REMOTE_DIR, PathBuf::new(), ConflictPolicy::Overwrite);
    let snap = wait_job(&mgr, id).await;
    assert_eq!(snap.state, JobState::Done, "首推失败: {:?}", snap.error);
    assert_eq!(snap.percent, 100);
    assert_eq!(snap.copied_bytes, 64 * 1024 + 256 * 1024 + 128 * 1024);

    let leftovers = adb.shell(&serial, &format!("find {} -name '.sl-part-*'", shq(REMOTE_DIR))).await.unwrap();
    assert!(leftovers.trim().is_empty(), "发现 .part 残留: {leftovers}");

    for (f, rel) in [(&f_a, "a.txt"), (&f_zh, "中文名 文件.mp4"), (&f_b, "sub/nested/b.bin")] {
        assert_eq!(local_md5(f), remote_md5(&adb, &serial, &remote(rel)).await, "内容不一致: {rel}");
    }

    // ---- 2. Rename 再推：生成 (1) 副本，不覆盖 ----
    let id = mgr.enqueue(JobKind::Push, &serial, files.clone(), rels.clone(), REMOTE_DIR, PathBuf::new(), ConflictPolicy::Rename);
    let snap = wait_job(&mgr, id).await;
    assert_eq!(snap.state, JobState::Done, "Rename 推送失败: {:?}", snap.error);
    let listing = adb.shell(&serial, &format!("ls {}", shq(REMOTE_DIR))).await.unwrap();
    assert!(listing.contains("a (1).txt"), "Rename 未生效: {listing}");
    assert!(listing.contains("中文名 文件 (1).mp4"), "中文名 Rename 未生效: {listing}");
    let listing_nested = adb.shell(&serial, &format!("ls {}", shq(&remote("sub/nested")))).await.unwrap();
    assert!(listing_nested.contains("b (1).bin"), "子目录 Rename 未生效: {listing_nested}");
    assert_eq!(local_md5(&f_b), remote_md5(&adb, &serial, &remote("sub/nested/b (1).bin")).await);

    // ---- 3. Skip 再推：全部跳过，不产生新文件 ----
    let id = mgr.enqueue(JobKind::Push, &serial, files.clone(), rels.clone(), REMOTE_DIR, PathBuf::new(), ConflictPolicy::Skip);
    let snap = wait_job(&mgr, id).await;
    assert_eq!(snap.state, JobState::Done, "Skip 推送失败: {:?}", snap.error);
    let listing = adb.shell(&serial, &format!("ls {}", shq(REMOTE_DIR))).await.unwrap();
    assert!(!listing.contains("a (2).txt"), "Skip 误产生新文件: {listing}");
    assert_eq!(listing.matches("a").count(), 2, "Skip 后顶层应仍是 2 个 a*.txt: {listing}");

    // ---- 清理 ----
    adb.shell(&serial, &format!("rm -rf {}", shq(REMOTE_DIR))).await.unwrap();
    let _ = std::fs::remove_dir_all(&local);
}

#[tokio::test]
async fn usb_driver_status_smoke() {
    match screenlink_core::usb::status() {
        Ok(st) => println!("usb status: {}", serde_json::to_string(&st).unwrap()),
        Err(e) => panic!("usb::status 失败: {e}"),
    }
}
