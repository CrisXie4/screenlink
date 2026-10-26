//! USB 驱动识别与修复（Windows）。
//!
//! 老电脑（尤其 Win7/早期 Win10）接安卓手机时常见两类问题：
//! 1. ADB 接口没有绑定 WinUSB 驱动（设备管理器黄色叹号，CM_PROB 28），
//!    `adb devices` 完全看不到设备 —— 需要 Google USB Driver；
//! 2. 绑定了错误的厂商驱动（如"便携设备"MTP 驱动顶替 ADB 接口）。
//!
//! 检测用 PowerShell `Get-PnpDevice`（系统自带，无需额外依赖）；
//! 修复走官方 Google USB Driver（dl.google.com 固定版本 URL），
//! 下载解压后用 `pnputil /add-driver /install` 提权安装（触发 UAC）。

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::path::PathBuf;

/// 常见 Android 厂商 USB VID（大小写不敏感比对）
const ANDROID_VIDS: &[&str] = &[
    "18D1", // Google / AOSP / 通用调试
    "2717", // 小米 / Redmi
    "04E8", // 三星
    "12D1", // 华为
    "12D2", // 华为（部分型号）
    "22D9", // OPPO
    "2D95", // vivo（部分）
    "2A45", // 一加
    "0FCE", // 索尼
    "0BB4", // HTC
    "1004", // LG
    "19D2", // 中兴
    "1782", // 展锐设备
    "0E8D", // 联发科调试口
    "1F3A", // Allwinner
];

/// Google USB Driver r13（官方固定版本 URL，最后一个维护版本，覆盖全部 ADB 接口）
const DRIVER_URL: &str = "https://dl.google.com/android/repository/usb_driver_r13-windows.zip";
const DRIVER_INF: &str = "android_winusb.inf";

#[derive(Debug, Clone, Serialize)]
pub struct UsbDevice {
    pub name: String,
    pub instance_id: String,
    pub vid: String,
    /// OK | Error | Degraded | Unknown（PnP 状态）
    pub status: String,
    /// CM_PROB 代码（28 = 未安装驱动）
    pub problem: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsbStatus {
    pub devices: Vec<UsbDevice>,
    /// 检测到疑似安卓设备但驱动状态异常（应提示安装驱动）
    pub missing_driver: bool,
}

#[cfg(windows)]
pub fn status() -> Result<UsbStatus> {
    // 只枚举 USB 总线设备：全量 PresentOnly 有数百条，既慢又刷管道
    let out = run_ps(
        "Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'USB\\*' } | \
         Select-Object Status,Problem,FriendlyName,InstanceId | ConvertTo-Json -Compress",
    )?;
    let text = out.trim();
    if text.is_empty() {
        return Ok(UsbStatus { devices: vec![], missing_driver: false });
    }
    // 单设备时 PowerShell 输出对象而非数组 —— 统一成数组解析
    let parsed: serde_json::Value = serde_json::from_str(&format!(
        "{}",
        if text.starts_with('[') { text.to_string() } else { format!("[{text}]") }
    ))
    .context("解析 Get-PnpDevice 输出失败")?;

    let mut devices = Vec::new();
    for item in parsed.as_array().cloned().unwrap_or_default() {
        let instance = item.get("InstanceId").and_then(|v| v.as_str()).unwrap_or("");
        let name = item.get("FriendlyName").and_then(|v| v.as_str()).unwrap_or("");
        // 只看 USB 总线上的设备；VID 匹配安卓厂商，或名称含 Android/ADB（兜底冷门厂商）
        let upper = instance.to_ascii_uppercase();
        if !upper.starts_with("USB\\") {
            continue;
        }
        let vid = extract_vid(&upper);
        let name_hit = name.contains("Android") || name.contains("ADB");
        let vid_hit = vid.as_deref().map(|v| ANDROID_VIDS.contains(&v)).unwrap_or(false);
        if !vid_hit && !name_hit {
            continue;
        }
        devices.push(UsbDevice {
            name: name.to_string(),
            instance_id: instance.to_string(),
            vid: vid.unwrap_or_default(),
            status: item.get("Status").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string(),
            problem: item.get("Problem").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        });
    }
    let missing_driver = devices
        .iter()
        .any(|d| d.status != "OK" && (d.problem == 28 || d.problem == 0));
    Ok(UsbStatus { devices, missing_driver })
}

#[cfg(not(windows))]
pub fn status() -> Result<UsbStatus> {
    Ok(UsbStatus { devices: vec![], missing_driver: false })
}

/// 下载并安装 Google USB Driver（全程 PowerShell，触发一次 UAC 提权确认）。
/// 成功返回提示文案；失败带原因。
#[cfg(windows)]
pub fn install_google_usb_driver() -> Result<String> {
    let tmp = std::env::temp_dir().join("screenlink-usb-driver");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).context("创建临时目录失败")?;
    let zip = tmp.join("usb_driver.zip");

    eprintln!("[sl-usb] 下载 Google USB Driver…");
    run_ps(&format!(
        "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
         Invoke-WebRequest -Uri '{DRIVER_URL}' -OutFile '{}' -UseBasicParsing",
        zip.to_string_lossy().replace('\'', "''")
    ))
    .map_err(|e| anyhow!("下载驱动失败（请检查网络）: {e}"))?;

    eprintln!("[sl-usb] 解压…");
    run_ps(&format!(
        "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
        zip.to_string_lossy(),
        tmp.to_string_lossy()
    ))?;

    let inf = find_inf(&tmp).ok_or_else(|| anyhow!("驱动包中未找到 {DRIVER_INF}"))?;

    eprintln!("[sl-usb] 提权安装（请在弹出的 UAC 窗口确认）…");
    // 提权脚本：pnputil 安装 + 结果落盘（UAC 子进程的退出码拿不到，用文件回传）
    let exit_file = tmp.join("install-exit.txt");
    let ps1 = tmp.join("sl-install-driver.ps1");
    let inf_q = inf.to_string_lossy().replace('\'', "''");
    std::fs::write(
        &ps1,
        format!(
            "$ErrorActionPreference = 'Stop'\n\
             $code = 1\n\
             try {{\n\
             \x20   & pnputil.exe /add-driver '{inf_q}' /install | Out-Null\n\
             \x20   $code = $LASTEXITCODE\n\
             }} catch {{ $code = 1 }}\n\
             Set-Content -Path '{exit_q}' -Value $code -Encoding Ascii\n",
            inf_q = inf_q,
            exit_q = exit_file.to_string_lossy().replace('\'', "''"),
        ),
    )?;
    let _ = std::fs::remove_file(&exit_file);
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden \
                 -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','{}'",
                ps1.to_string_lossy().replace('\'', "''")
            ),
        ])
        .output()
        .context("启动提权安装失败")?;
    if !status.status.success() && !exit_file.exists() {
        return Err(anyhow!("安装被取消（UAC 未确认）"));
    }
    let code = std::fs::read_to_string(&exit_file)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(-1);
    if code != 0 {
        return Err(anyhow!("pnputil 安装失败（exit {code}）"));
    }
    // 触发一次重新枚举，adbd 立即可见
    let _ = run_ps("pnputil.exe /scan-devices");
    Ok("驱动安装完成。请重新插拔数据线，或直接重新扫描设备".into())
}

#[cfg(not(windows))]
pub fn install_google_usb_driver() -> Result<String> {
    Err(anyhow!("USB 驱动修复仅 Windows 需要"))
}

/// 提取 `USB\VID_18D1&PID_4EE7\...` 中的 VID
fn extract_vid(instance: &str) -> Option<String> {
    let rest = instance.strip_prefix("USB\\")?;
    let vid = rest.strip_prefix("VID_")?;
    Some(vid.get(..4)?.to_string())
}

#[cfg(windows)]
fn find_inf(dir: &PathBuf) -> Option<PathBuf> {
    fn walk(d: &std::path::Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.file_name().map(|n| n == DRIVER_INF).unwrap_or(false) {
                    out.push(p);
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found.into_iter().next()
}

#[cfg(windows)]
fn run_ps(script: &str) -> Result<String> {
    // 控制台默认代码页（GBK）会把中文 FriendlyName 打成 mojibake，强制 UTF-8
    let full = format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {script}");
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &full])
        .output()
        .context("启动 PowerShell 失败")?;
    if !out.status.success() {
        return Err(anyhow!(
            "PowerShell 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
