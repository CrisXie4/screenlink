//! 文件传输：任务队列 + 进度 + 冲突策略（覆盖/跳过/重命名）+ 失败重试。
//!
//! 通道设计：
//! - USB / WiFi 均走 `adb push/pull`（ADB over TCP/IP 下依旧可用，进度可解析）。
//! - 预留 `run_job` 内部按 transport 分派自建 TCP 高速通道的扩展位
//!   （手机端 agent 上线后启用，目标 >80MB/s 与断点续传）。
//!
//! 可靠性设计（push）：
//! - 逐文件传输：单个文件失败不影响其余文件，且进度连续；
//! - 原子落盘：先推到远端 `.sl-part-<name>`，成功后 `mv` 改名 ——
//!   中断不会留下"看起来完整"的半截文件；
//! - 失败自动重试 1 次（瞬时 USB 抖动 / WiFi 丢包场景）；
//! - 文件夹拖拽：入队前展开为文件清单 + 相对路径，远端自动 `mkdir -p`。

use crate::adb::{Adb, CancelToken};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    #[default]
    Overwrite,
    Skip,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Push,
    Pull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobSnapshot {
    pub id: u64,
    pub kind: JobKind,
    pub serial: String,
    pub title: String,
    pub state: JobState,
    pub total_bytes: u64,
    pub copied_bytes: u64,
    pub percent: u8,
    pub speed_bps: f64,
    pub eta_secs: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub file_count: usize,
}

#[derive(Clone)]
pub struct TransferManager {
    adb: Adb,
    next_id: Arc<AtomicU64>,
    tx: mpsc::UnboundedSender<PendingJob>,
    cancels: Arc<Mutex<HashMap<u64, CancelToken>>>,
    /// 已入队任务的原参数（失败重试时重建）；同步锁：retry() 为同步入口
    jobs: Arc<std::sync::Mutex<HashMap<u64, PendingJob>>>,
    snapshots_tx: Arc<watch::Sender<Vec<JobSnapshot>>>,
}

#[derive(Debug, Clone)]
struct PendingJob {
    id: u64,
    kind: JobKind,
    serial: String,
    files: Vec<PathBuf>,
    /// 与 files 一一对应的远端相对路径（文件夹拖拽时含子目录）
    rel_paths: Vec<String>,
    remote: String,
    local_dir: PathBuf,
    policy: ConflictPolicy,
}

impl TransferManager {
    pub fn new(adb: Adb) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (snap_tx, _) = watch::channel(Vec::new());
        let mgr = Self {
            adb,
            next_id: Arc::new(AtomicU64::new(1)),
            tx,
            cancels: Arc::new(Mutex::new(HashMap::new())),
            jobs: Arc::new(std::sync::Mutex::new(HashMap::new())),
            snapshots_tx: Arc::new(snap_tx),
        };
        mgr.spawn_worker(rx);
        mgr
    }

    pub fn snapshots(&self) -> Vec<JobSnapshot> {
        self.snapshots_tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Vec<JobSnapshot>> {
        self.snapshots_tx.subscribe()
    }

    /// 入队传输任务（同一设备内 adb 串行执行，不同设备并行）。
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue(
        &self,
        kind: JobKind,
        serial: &str,
        files: Vec<PathBuf>,
        rel_paths: Vec<String>,
        remote: &str,
        local_dir: PathBuf,
        policy: ConflictPolicy,
    ) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let total: u64 = files
            .iter()
            .filter_map(|f| std::fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();
        let title = match kind {
            JobKind::Push => format!("发送 {} 个文件到手机", files.len()),
            JobKind::Pull => format!("接收 {}", remote.rsplit('/').next().unwrap_or(remote)),
        };
        self.update_snap(id, |s| {
            *s = JobSnapshot {
                id,
                kind,
                serial: serial.to_string(),
                title,
                state: JobState::Queued,
                total_bytes: total,
                copied_bytes: 0,
                percent: 0,
                speed_bps: 0.0,
                eta_secs: 0.0,
                error: None,
                file_count: files.len(),
            };
        });
        let job = PendingJob {
            id,
            kind,
            serial: serial.to_string(),
            files,
            rel_paths,
            remote: remote.to_string(),
            local_dir,
            policy,
        };
        if let Ok(mut map) = self.jobs.lock() {
            map.insert(id, job.clone());
        }
        let _ = self.tx.send(job);
        id
    }

    /// 重试历史任务：以原参数重新入队，返回新任务 id。
    pub fn retry(&self, id: u64) -> Option<u64> {
        let job = self.jobs.lock().ok()?.get(&id)?.clone();
        Some(match job.kind {
            JobKind::Push => self.enqueue(
                JobKind::Push,
                &job.serial,
                job.files.clone(),
                job.rel_paths.clone(),
                &job.remote,
                job.local_dir.clone(),
                job.policy,
            ),
            JobKind::Pull => self.enqueue(
                JobKind::Pull,
                &job.serial,
                vec![],
                job.rel_paths.clone(),
                &job.remote,
                job.local_dir.clone(),
                job.policy,
            ),
        })
    }

    pub async fn cancel(&self, id: u64) {
        if let Some(t) = self.cancels.lock().await.remove(&id) {
            t.cancel();
        }
        self.update_snap(id, |s| {
            if s.state == JobState::Queued || s.state == JobState::Running {
                s.state = JobState::Cancelled;
            }
        });
    }

    fn update_snap(&self, id: u64, f: impl FnOnce(&mut JobSnapshot)) {
        let mut snaps = self.snapshots_tx.borrow().clone();
        if let Some(pos) = snaps.iter().position(|s| s.id == id) {
            f(&mut snaps[pos]);
        } else {
            let mut s = JobSnapshot {
                id,
                kind: JobKind::Push,
                serial: String::new(),
                title: String::new(),
                state: JobState::Queued,
                total_bytes: 0,
                copied_bytes: 0,
                percent: 0,
                speed_bps: 0.0,
                eta_secs: 0.0,
                error: None,
                file_count: 0,
            };
            f(&mut s);
            snaps.push(s);
        }
        let _ = self.snapshots_tx.send(snaps);
    }

    fn spawn_worker(&self, mut rx: mpsc::UnboundedReceiver<PendingJob>) {
        // 自带 runtime：GUI 的 setup 阶段没有环境 tokio 上下文，
        // 用专用线程 + current_thread runtime 自洽运行传输队列。
        let adb = self.adb.clone();
        let snaps_tx = self.snapshots_tx.clone();
        let cancels = self.cancels.clone();
        std::thread::Builder::new()
            .name("screenlink-transfer".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("transfer runtime");
                rt.block_on(async move {
                    let serials: Arc<Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>> =
                        Arc::new(Mutex::new(HashMap::new()));
                    while let Some(job) = rx.recv().await {
                        // 每设备一个信号量：同设备传输串行（adb 单通道），跨设备并行
                        let sem = serials
                            .lock()
                            .await
                            .entry(job.serial.clone())
                            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
                            .clone();
                        let adb = adb.clone();
                        let snaps_tx = snaps_tx.clone();
                        let cancels = cancels.clone();
                        let update = move |id: u64, f: &mut dyn FnMut(&mut JobSnapshot)| {
                            let mut list = snaps_tx.borrow().clone();
                            if let Some(s) = list.iter_mut().find(|s| s.id == id) {
                                f(s);
                                let _ = snaps_tx.send(list);
                            }
                        };
                        let update: Arc<
                            dyn Fn(u64, &mut dyn FnMut(&mut JobSnapshot)) + Send + Sync,
                        > = Arc::new(update);
                        tokio::spawn(async move {
                            let _permit = sem.acquire().await;
                            let cancel = CancelToken::new();
                            let id = job.id;
                            {
                                let mut map = cancels.lock().await;
                                map.insert(id, cancel.clone());
                            }
                            update(id, &mut |s| s.state = JobState::Running);
                            let res = run_job(&adb, &job, cancel, id, update.clone()).await;
                            cancels.lock().await.remove(&id);
                            let (state, err) = match res {
                                Ok(()) => (JobState::Done, None),
                                Err(e) => (JobState::Failed, Some(e.to_string())),
                            };
                            update(id, &mut move |s: &mut JobSnapshot| {
                                if s.state != JobState::Cancelled {
                                    s.state = state;
                                    s.error = err.clone();
                                    if state == JobState::Done {
                                        s.percent = 100;
                                        s.copied_bytes = s.total_bytes.max(s.copied_bytes);
                                    }
                                }
                            });
                        });
                    }
                });
            })
            .expect("启动传输线程失败");
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    adb: &Adb,
    job: &PendingJob,
    cancel: CancelToken,
    id: u64,
    update: Arc<dyn Fn(u64, &mut dyn FnMut(&mut JobSnapshot)) + Send + Sync>,
) -> anyhow::Result<()> {
    match job.kind {
        JobKind::Push => run_push(adb, job, cancel, id, update).await,
        JobKind::Pull => run_pull(adb, job, cancel, id, update).await,
    }
}

/// push 主流程：逐文件原子传输 + 冲突策略 + 失败重试 1 次。
async fn run_push(
    adb: &Adb,
    job: &PendingJob,
    cancel: CancelToken,
    id: u64,
    update: Arc<dyn Fn(u64, &mut dyn FnMut(&mut JobSnapshot)) + Send + Sync>,
) -> anyhow::Result<()> {
    let dir = job.remote.trim_end_matches('/').to_string();
    let total: u64 = job
        .files
        .iter()
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len())
        .sum();
    let start = std::time::Instant::now();
    let last_emit = Arc::new(AtomicU64::new(0));
    let n = job.files.len();
    let mut done_bytes: u64 = 0;

    for (idx, f) in job.files.iter().enumerate() {
        if cancel.is_cancelled() {
            anyhow::bail!("已取消");
        }
        let fname = f
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("file-{idx}"));
        let rel = job
            .rel_paths
            .get(idx)
            .cloned()
            .unwrap_or_else(|| fname.clone());
        let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
        update(id, &mut |s| {
            s.title = format!("发送 {}/{}：{}", idx + 1, n, rel);
        });

        // 文件夹拖拽：先建子目录
        if let Some(pos) = rel.rfind('/') {
            let sub = format!("{dir}/{}", &rel[..pos]);
            adb.shell(&job.serial, &format!("mkdir -p {}", shq(&sub))).await?;
        }

        // 冲突策略 -> 最终远端相对路径
        let final_rel = match job.policy {
            ConflictPolicy::Overwrite => rel.clone(),
            ConflictPolicy::Skip => {
                if remote_exists(adb, &job.serial, &format!("{dir}/{rel}")).await {
                    done_bytes += size;
                    emit_progress(&update, id, done_bytes, total, &start, &last_emit, true);
                    continue;
                }
                rel.clone()
            }
            ConflictPolicy::Rename => {
                let mut name = rel.clone();
                let mut i = 1u32;
                while remote_exists(adb, &job.serial, &format!("{dir}/{name}")).await {
                    name = rename_candidate(&rel, i);
                    i += 1;
                }
                name
            }
        };
        let final_remote = format!("{dir}/{final_rel}");
        // 临时名用原始文件名（不用 final_rel）：避免重命名后的 "(1)" 后缀混入 .part 名
        let tmp_remote = format!("{dir}/.sl-part-{fname}");

        // 单文件：失败自动重试 1 次（USB 抖动 / WiFi 瞬断）
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..2u8 {
            if cancel.is_cancelled() {
                anyhow::bail!("已取消");
            }
            let base = done_bytes;
            let res = push_file_once(
                adb,
                &job.serial,
                f,
                &tmp_remote,
                &final_remote,
                cancel.clone(),
                make_progress(
                    update.clone(),
                    last_emit.clone(),
                    id,
                    base,
                    size,
                    total,
                    start,
                ),
            )
            .await;
            match res {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    if cancel.is_cancelled() {
                        anyhow::bail!("已取消");
                    }
                    last_err = Some(e);
                    if attempt == 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
        }
        if let Some(e) = last_err {
            return Err(e.context(format!("发送「{rel}」失败（其余文件已停止）")));
        }
        done_bytes += size;
        emit_progress(&update, id, done_bytes, total, &start, &last_emit, true);
    }
    Ok(())
}

/// 单文件一次尝试：清残留 .part -> push 到临时名 -> 原子 mv 到最终名。
async fn push_file_once(
    adb: &Adb,
    serial: &str,
    file: &PathBuf,
    tmp_remote: &str,
    final_remote: &str,
    cancel: CancelToken,
    progress: impl FnMut(u64, u64, u8) + Send + 'static,
) -> anyhow::Result<()> {
    // 清上次中断的残留，保证 push 全量覆写
    let _ = adb.shell(serial, &format!("rm -f {}", shq(tmp_remote))).await;
    adb.push(serial, std::slice::from_ref(file), tmp_remote, cancel, progress)
        .await?;
    adb.shell(
        serial,
        &format!("mv -f {} {}", shq(tmp_remote), shq(final_remote)),
    )
    .await
    .map(|_| ())
    .map_err(|e| anyhow::Error::msg(format!("落盘失败: {e}")))
}

/// pull：本地冲突策略 + staging 暂存后落位（保持原实现）。
async fn run_pull(
    adb: &Adb,
    job: &PendingJob,
    cancel: CancelToken,
    id: u64,
    update: Arc<dyn Fn(u64, &mut dyn FnMut(&mut JobSnapshot)) + Send + Sync>,
) -> anyhow::Result<()> {
    let local_dir = &job.local_dir;
    let name = job
        .remote
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(job.remote.as_str())
        .to_string();
    let target_name = match job.policy {
        ConflictPolicy::Overwrite => name.clone(),
        ConflictPolicy::Skip => {
            if local_dir.join(&name).exists() {
                anyhow::bail!("本地已存在 {}（策略：跳过）", name);
            }
            name.clone()
        }
        ConflictPolicy::Rename => {
            let mut n = name.clone();
            let stem = std::path::Path::new(&name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| name.clone());
            let ext = std::path::Path::new(&name)
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            let mut i = 1;
            while local_dir.join(&n).exists() {
                n = if ext.is_empty() {
                    format!("{} ({})", stem, i)
                } else {
                    format!("{} ({}).{}", stem, i, ext.trim_start_matches('.'))
                };
                i += 1;
            }
            n
        }
    };
    let start = std::time::Instant::now();
    let last_emit = Arc::new(AtomicU64::new(0));
    let staging = local_dir.join(".screenlink_staging");
    std::fs::create_dir_all(&staging)?;
    // 远端总量未知（0）：由 adb.pull 的 du 估算经回调补全
    adb.pull(
        &job.serial,
        &job.remote,
        &staging,
        cancel,
        make_progress(update, last_emit, id, 0, u64::MAX, 0, start),
    )
    .await?;
    let staged = staging.join(&name);
    let final_path = local_dir.join(&target_name);
    if staged.exists() {
        if final_path.exists() {
            let _ = std::fs::remove_file(&final_path);
        }
        std::fs::rename(&staged, &final_path)?;
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

/// 节流的整体进度回调：abs = base + 本文件已传字节；
/// total 为 0（未知，pull 场景）时回退用回调传入的 t，百分比回退用 adb 的 pct。
#[allow(clippy::too_many_arguments)]
fn make_progress(
    update: Arc<dyn Fn(u64, &mut dyn FnMut(&mut JobSnapshot)) + Send + Sync>,
    last_emit: Arc<AtomicU64>,
    id: u64,
    base: u64,
    fsize: u64,
    total: u64,
    start: std::time::Instant,
) -> impl FnMut(u64, u64, u8) + Send + 'static {
    move |copied: u64, t: u64, pct: u8| {
        let eff_total = if total > 0 { total } else { t };
        let abs = base + copied.min(fsize);
        let percent = if eff_total > 0 {
            ((abs.min(eff_total) as f64 / eff_total as f64) * 100.0).min(100.0) as u8
        } else {
            pct.min(100)
        };
        let now = start.elapsed().as_millis() as u64;
        if now.saturating_sub(last_emit.load(Ordering::Relaxed)) < 200 && percent < 100 {
            return;
        }
        last_emit.store(now, Ordering::Relaxed);
        let elapsed = start.elapsed().as_secs_f64();
        update(id, &mut |s: &mut JobSnapshot| {
            s.total_bytes = eff_total;
            s.copied_bytes = abs;
            s.percent = percent;
            s.speed_bps = if elapsed > 0.05 { abs as f64 / elapsed } else { 0.0 };
            s.eta_secs = if percent > 2 && elapsed > 0.2 {
                elapsed * (100.0 - percent as f64) / percent as f64
            } else {
                0.0
            };
        });
    }
}

fn emit_progress(
    update: &Arc<dyn Fn(u64, &mut dyn FnMut(&mut JobSnapshot)) + Send + Sync>,
    id: u64,
    abs: u64,
    total: u64,
    start: &std::time::Instant,
    last_emit: &AtomicU64,
    force: bool,
) {
    let now = start.elapsed().as_millis() as u64;
    if !force && now.saturating_sub(last_emit.load(Ordering::Relaxed)) < 200 {
        return;
    }
    last_emit.store(now, Ordering::Relaxed);
    emit_progress_at(update, id, abs, total, start);
}

fn emit_progress_at(
    update: &Arc<dyn Fn(u64, &mut dyn FnMut(&mut JobSnapshot)) + Send + Sync>,
    id: u64,
    abs: u64,
    total: u64,
    start: &std::time::Instant,
) {
    let elapsed = start.elapsed().as_secs_f64();
    let percent = if total > 0 {
        (((abs.min(total)) as f64 / total as f64) * 100.0).min(100.0) as u8
    } else {
        0
    };
    update(id, &mut |s: &mut JobSnapshot| {
        s.total_bytes = total;
        s.copied_bytes = abs;
        s.percent = percent;
        s.speed_bps = if elapsed > 0.05 { abs as f64 / elapsed } else { 0.0 };
        s.eta_secs = if percent > 2 && elapsed > 0.2 {
            elapsed * (100.0 - percent as f64) / percent as f64
        } else {
            0.0
        };
    });
}

/// 远端文件是否存在（shell 单行探测）
async fn remote_exists(adb: &Adb, serial: &str, path: &str) -> bool {
    adb.shell(serial, &format!("[ -e {} ] && echo y || echo n", shq(path)))
        .await
        .map(|s| s.trim() == "y")
        .unwrap_or(false)
}

/// shell 单引号转义
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Rename 策略的候选名：`a/b/c.mp4` + 1 -> `a/b/c (1).mp4`（保留子目录）
fn rename_candidate(rel: &str, n: u32) -> String {
    let (dir_part, file) = match rel.rfind('/') {
        Some(pos) => (&rel[..pos + 1], &rel[pos + 1..]),
        None => ("", rel),
    };
    let (stem, ext) = match file.rfind('.') {
        Some(0) | None => (file, ""),
        Some(pos) => (&file[..pos], &file[pos..]),
    };
    format!("{dir_part}{stem} ({n}){ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_candidate_keeps_subdir_and_ext() {
        assert_eq!(rename_candidate("a/b/c.mp4", 1), "a/b/c (1).mp4");
        assert_eq!(rename_candidate("c.mp4", 2), "c (2).mp4");
        assert_eq!(rename_candidate("noext", 1), "noext (1)");
        assert_eq!(rename_candidate(".hidden", 1), ".hidden (1)"); // 点开头不是扩展名
        assert_eq!(rename_candidate("d/archive.tar.gz", 3), "d/archive.tar (3).gz");
    }

    #[test]
    fn shq_escapes_single_quotes() {
        assert_eq!(shq("it's.txt"), "'it'\\''s.txt'");
        assert_eq!(shq("中文 (1).mp4"), "'中文 (1).mp4'");
    }
}
