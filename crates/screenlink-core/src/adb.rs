//! ADB 封装：进程管理、设备列表、连接/配对、文件推拉、shell、截图、UI dump。
//!
//! 设计要点：
//! - 不绑定 adb 版本实现细节，全部通过子进程 + 输出解析（adb 二进制由平台安装）。
//! - 所有交互均为 async（tokio::process），Windows 下隐藏控制台窗口。
//! - 传输进度：解析 `adb push/pull` 的 stderr 百分比行，节流后回调。

use crate::util::no_window;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;

pub const SERVER_JAR_NAME: &str = "scrcpy-server-v2.7.jar";

/// 取消令牌：跨任务共享，置位后唤醒等待方（用于杀掉传输子进程）。
#[derive(Clone, Default)]
pub struct CancelToken(Arc<(std::sync::atomic::AtomicBool, tokio::sync::Notify)>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0 .0.store(true, Ordering::SeqCst);
        self.0 .1.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.0 .0.load(Ordering::SeqCst)
    }
    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            let fut = self.0 .1.notified();
            tokio::pin!(fut);
            // 竞态防护：注册等待前再查一次
            if self.is_cancelled() {
                return;
            }
            fut.await;
        }
    }
}

type Ordering = std::sync::atomic::Ordering;

#[derive(Debug, Clone)]
pub struct Adb {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdbOut {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl AdbOut {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

impl Adb {
    /// 依次尝试：环境变量 SCREENLINK_ADB -> PATH -> Android SDK 默认位置。
    pub fn discover() -> Result<Adb> {
        if let Ok(p) = std::env::var("SCREENLINK_ADB") {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Ok(Adb { path: p });
            }
        }
        if let Ok(out) = which("adb") {
            return Ok(Adb { path: PathBuf::from(out) });
        }
        #[cfg(windows)]
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let p = PathBuf::from(local)
                .join("Android")
                .join("Sdk")
                .join("platform-tools")
                .join("adb.exe");
            if p.is_file() {
                return Ok(Adb { path: p });
            }
        }
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        for cand in [
            PathBuf::from(&home).join("Android/Sdk/platform-tools/adb"),
            PathBuf::from("/usr/bin/adb"),
            PathBuf::from("/opt/homebrew/bin/adb"),
        ] {
            if cand.is_file() {
                return Ok(Adb { path: cand });
            }
        }
        Err(anyhow!(
            "未找到 adb。请安装 Android platform-tools，或设置 SCREENLINK_ADB 指向 adb.exe"
        ))
    }

    pub async fn run(&self, args: &[&str]) -> Result<AdbOut> {
        let mut cmd = Command::new(&self.path);
        cmd.args(args);
        no_window(&mut cmd);
        let out = cmd.output().await.context("启动 adb 进程失败")?;
        Ok(AdbOut {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    pub async fn server_version(&self) -> Result<String> {
        let out = self.run(&["version"]).await?;
        let line = out
            .stdout
            .lines()
            .next()
            .ok_or_else(|| anyhow!("adb version 无输出"))?;
        Ok(line.to_string())
    }

    /// `adb devices -l` 输出解析。
    pub async fn devices(&self) -> Result<Vec<crate::device::DeviceInfo>> {
        let out = self.run(&["devices", "-l"]).await?;
        let mut list = Vec::new();
        for line in out.stdout.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() || line.starts_with("List of") || line.starts_with('*') {
                continue;
            }
            if let Some(d) = crate::device::parse_devices_line(line) {
                list.push(d);
            }
        }
        Ok(list)
    }

    pub async fn shell(&self, serial: &str, cmd: &str) -> Result<String> {
        let out = self
            .run(&["-s", serial, "shell", cmd])
            .await
            .with_context(|| format!("adb shell {cmd} 失败"))?;
        if !out.ok() && out.stdout.is_empty() {
            return Err(anyhow!("adb shell {cmd}: {}", out.stderr.trim()));
        }
        Ok(out.stdout)
    }

    pub async fn connect(&self, addr: &str) -> Result<String> {
        let out = self.run(&["connect", addr]).await?;
        let msg = out.stdout.trim().to_string();
        if msg.contains("connected to") || msg.contains("already connected") {
            Ok(msg)
        } else {
            Err(anyhow!("连接 {addr} 失败: {msg}{}", out.stderr.trim()))
        }
    }

    pub async fn disconnect(&self, addr: &str) -> Result<()> {
        let _ = self.run(&["disconnect", addr]).await?;
        Ok(())
    }

    /// 安卓 11+ 无线调试配对：`adb pair ip:port code`
    pub async fn pair(&self, addr: &str, code: &str) -> Result<String> {
        let out = self.run(&["pair", addr, code]).await?;
        let msg = format!("{} {}", out.stdout.trim(), out.stderr.trim());
        if msg.contains("Success") || msg.contains("success") {
            Ok(msg.trim().to_string())
        } else {
            Err(anyhow!("配对失败: {}", msg.trim()))
        }
    }

    /// USB 在线时切到 TCP/IP 监听（为拔线后 WiFi 重连做准备）。
    pub async fn tcpip(&self, serial: &str, port: u16) -> Result<String> {
        let out = self.run(&["-s", serial, "tcpip", &port.to_string()]).await?;
        if out.ok() {
            Ok(out.stdout.trim().to_string())
        } else {
            Err(anyhow!("tcpip 失败: {}", out.stderr.trim()))
        }
    }

    pub async fn usb(&self, serial: &str) -> Result<()> {
        self.run(&["-s", serial, "usb"]).await?;
        Ok(())
    }

    /// mDNS 服务发现：安卓 11+ 无线调试会广播 _adb-tls-connect/_adb-tls-pairing。
    pub async fn mdns_services(&self) -> Result<Vec<crate::device::MdnsService>> {
        let out = self.run(&["mdns", "services"]).await?;
        let mut list = Vec::new();
        for line in out.stdout.lines() {
            if let Some(s) = crate::device::parse_mdns_line(line) {
                list.push(s);
            }
        }
        Ok(list)
    }

    pub async fn forward(&self, serial: &str, local: &str, target: &str) -> Result<()> {
        let out = self.run(&["-s", serial, "forward", local, target]).await?;
        if out.ok() {
            Ok(())
        } else {
            Err(anyhow!("forward 失败: {}", out.stderr.trim()))
        }
    }

    pub async fn forward_remove_all(&self, serial: &str) -> Result<()> {
        let _ = self.run(&["-s", serial, "forward", "--remove-all"]).await;
        Ok(())
    }

    pub async fn reverse(&self, serial: &str, remote: &str, local: &str) -> Result<()> {
        let out = self.run(&["-s", serial, "reverse", remote, local]).await?;
        if out.ok() {
            Ok(())
        } else {
            Err(anyhow!("reverse 失败: {}", out.stderr.trim()))
        }
    }

    /// 获取设备 wlan0 IP（用于拔线后 adb connect 重连）。
    pub async fn wifi_ip(&self, serial: &str) -> Option<String> {
        let out = self
            .shell(serial, "ip -f inet addr show wlan0")
            .await
            .ok()?;
        for line in out.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("inet ") {
                let ip = rest.split_whitespace().next()?;
                if ip.contains('.') {
                    return Some(ip.to_string());
                }
            }
        }
        None
    }

    /// 电量（0-100）。
    pub async fn battery(&self, serial: &str) -> Option<u8> {
        let out = self.shell(serial, "dumpsys battery").await.ok()?;
        for line in out.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("level:") {
                return v.trim().parse().ok();
            }
        }
        None
    }

    /// Push 多个文件到设备目录，progress 回调 (copied_bytes, total_bytes, percent)。
    /// cancel 置位后杀掉 adb 子进程。progress 闭包须为 'static（内部独立任务调用）。
    pub async fn push(
        &self,
        serial: &str,
        files: &[PathBuf],
        remote_dir: &str,
        cancel: CancelToken,
        mut progress: impl FnMut(u64, u64, u8) + Send + 'static,
    ) -> Result<Vec<String>> {
        let total: u64 = files
            .iter()
            .filter_map(|f| std::fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();
        let mut args = vec!["-s".to_string(), serial.to_string(), "push".to_string()];
        for f in files {
            args.push(f.to_string_lossy().into_owned());
        }
        args.push(remote_dir.to_string());
        self.run_with_progress(&args, total, cancel, progress).await?;
        // 计算目标路径（同目录同名；adb push 单文件到目录也是该规则）
        let remote_dir = remote_dir.trim_end_matches('/');
        let names: Vec<String> = files
            .iter()
            .map(|f| format!("{}/{}", remote_dir, file_name_of(f)))
            .collect();
        Ok(names)
    }

    /// Pull 设备路径（文件或目录）到本地目录。
    pub async fn pull(
        &self,
        serial: &str,
        remote: &str,
        local_dir: &Path,
        cancel: CancelToken,
        mut progress: impl FnMut(u64, u64, u8) + Send + 'static,
    ) -> Result<Vec<PathBuf>> {
        // 预估远端大小用于进度（失败则总量未知，进度按百分比输出解析）
        let total = self
            .shell(serial, &format!("du -sb '{}'", remote.replace('\'', "'\\''")))
            .await
            .ok()
            .and_then(|s| {
                s.lines()
                    .next()
                    .and_then(|l| l.split('\t').next())
                    .and_then(|n| n.trim().parse::<u64>().ok())
            })
            .unwrap_or(0);
        let args = vec![
            "-s".to_string(),
            serial.to_string(),
            "pull".to_string(),
            remote.to_string(),
            local_dir.to_string_lossy().into_owned(),
        ];
        self.run_with_progress(&args, total, cancel, progress).await?;
        let name = remote.trim_end_matches('/').rsplit('/').next().unwrap_or(remote);
        Ok(vec![local_dir.join(name)])
    }

    /// 运行 adb 命令并解析 stderr 中的 "[ 42%]" 进度行（adb push/pull 的进度输出）。
    async fn run_with_progress(
        &self,
        args: &[String],
        total: u64,
        cancel: CancelToken,
        mut progress: impl FnMut(u64, u64, u8) + Send + 'static,
    ) -> Result<()> {
        use tokio::io::AsyncReadExt;
        let mut cmd = Command::new(&self.path);
        cmd.args(args);
        no_window(&mut cmd);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().context("启动 adb 传输进程失败")?;
        let mut stderr = child.stderr.take().ok_or_else(|| anyhow!("无 stderr 管道"))?;
        let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("无 stdout 管道"))?;

        // stdout 排空任务（防管道阻塞）
        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut tmp).await {
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            buf
        });

        // 进度读取任务：progress 闭包经 Arc<Mutex> 共享，读取任务与收尾回调共用
        let shared: Arc<std::sync::Mutex<dyn FnMut(u64, u64, u8) + Send>> =
            Arc::new(std::sync::Mutex::new(progress));
        let shared2 = shared.clone();
        let prog_task = tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            let mut tail = String::new();
            let mut err_tail = String::new();
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        err_tail.push_str(&String::from_utf8_lossy(&chunk[..n]));
                        if err_tail.len() > 4096 {
                            err_tail = err_tail[err_tail.len() - 2048..].to_string();
                        }
                        tail.push_str(&String::from_utf8_lossy(&chunk[..n]));
                        while let Some(pos) = tail.find(']') {
                            let line = &tail[..=pos];
                            if let Some(pct_start) = line.find('[') {
                                let pct_str =
                                    line[pct_start + 1..pos].trim().trim_end_matches('%').trim();
                                if let Ok(p) = pct_str.parse::<u8>() {
                                    let p = p.min(100);
                                    let copied =
                                        if total > 0 { total * u64::from(p) / 100 } else { 0 };
                                    let mut f = shared2.lock().unwrap_or_else(|e| e.into_inner());
                                    f(copied, total, p);
                                }
                            }
                            tail.drain(..=pos);
                        }
                    }
                }
            }
            err_tail
        });
        let prog_task_err = prog_task;

        // 等待：进程退出 或 取消（取消即杀进程）
        let status = tokio::select! {
            st = child.wait() => st,
            _ = cancel.cancelled() => {
                child.start_kill();
                child.wait().await
            }
        };
        let err_tail = prog_task_err.await.unwrap_or_default();
        let _ = stdout_task.await;
        if cancel.is_cancelled() {
            return Err(anyhow!("已取消"));
        }
        let status = status.context("等待 adb 传输进程失败")?;
        if !status.success() {
            return Err(anyhow!(
                "adb 传输失败（exit {}）: {}",
                status.code().unwrap_or(-1),
                err_tail.trim()
            ));
        }
        {
            let mut f = shared.lock().unwrap_or_else(|e| e.into_inner());
            f(total, total, 100);
        }
        Ok(())
    }

    /// 截图（PNG 字节）。exec-out 避免 CRLF 污染。
    pub async fn screencap_png(&self, serial: &str) -> Result<Vec<u8>> {
        let mut cmd = Command::new(&self.path);
        cmd.args(["-s", serial, "exec-out", "screencap", "-p"]);
        no_window(&mut cmd);
        let out = cmd.output().await.context("screencap 失败")?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(anyhow!(
                "screencap 失败: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(out.stdout)
    }

    /// uiautomator dump：获取当前页面可交互元素（XML）。/dev/tty 直出避免拉临时文件。
    pub async fn ui_xml(&self, serial: &str) -> Result<String> {
        let out = self
            .run(&[
                "-s",
                serial,
                "exec-out",
                "uiautomator",
                "dump",
                "/dev/tty",
            ])
            .await?;
        let text = out.stdout.trim().to_string();
        if text.starts_with("<?xml") || text.starts_with("<hierarchy") {
            Ok(text)
        } else {
            Err(anyhow!(
                "uiautomator dump 失败: {}",
                if text.is_empty() { &out.stderr } else { &text }
            ))
        }
    }

    /// 当前前台窗口/Activity。
    pub async fn current_focus(&self, serial: &str) -> Option<String> {
        let out = self
            .shell(serial, "dumpsys window displays | grep -E mCurrentFocus")
            .await
            .ok()?;
        out.lines()
            .find_map(|l| l.split("mCurrentFocus=").nth(1).map(str::to_string))
            .map(|s| s.trim().to_string())
    }
}

fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn which(name: &str) -> Result<String> {
    // 简版 where/which：遍历 PATH
    let path = std::env::var("PATH").unwrap_or_default();
    let ext = if cfg!(windows) { ".exe" } else { "" };
    for dir in path.split(';').chain(path.split(':')) {
        if dir.is_empty() {
            continue;
        }
        let cand = PathBuf::from(dir).join(format!("{name}{ext}"));
        if cand.is_file() {
            return Ok(cand.to_string_lossy().into_owned());
        }
    }
    Err(anyhow!("{name} not found in PATH"))
}
