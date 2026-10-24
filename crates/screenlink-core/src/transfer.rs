//! 文件传输：任务队列 + 进度 + 冲突策略（覆盖/跳过/重命名）。
//!
//! 通道设计：
//! - USB / WiFi 均走 `adb push/pull`（ADB over TCP/IP 下依旧可用，进度可解析）。
//! - 预留 `run_job` 内部按 transport 分派自建 TCP 高速通道的扩展位
//!   （手机端 agent 上线后启用，目标 >80MB/s 与断点续传）。
//! - 冲突策略：push 时对远端同名文件处理；pull 时对本地同名文件处理。

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
    snapshots_tx: Arc<watch::Sender<Vec<JobSnapshot>>>,
}

struct PendingJob {
    id: u64,
    kind: JobKind,
    serial: String,
    files: Vec<PathBuf>,
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
    pub fn enqueue(
        &self,
        kind: JobKind,
        serial: &str,
        files: Vec<PathBuf>,
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
            JobKind::Push => format!(
                "发送 {} 个文件到手机",
                files.len()
            ),
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
        let _ = self.tx.send(PendingJob {
            id,
            kind,
            serial: serial.to_string(),
            files,
            remote: remote.to_string(),
            local_dir,
            policy,
        });
        id
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
    let start = std::time::Instant::now();
    let last_emit = std::cell::Cell::new(0u128);
    let make_progress = |update: Arc<
        dyn Fn(u64, &mut dyn FnMut(&mut JobSnapshot)) + Send + Sync,
    >| {
        move |copied: u64, total: u64, pct: u8| {
            let now = start.elapsed().as_millis();
            if now - last_emit.get() > 200 || pct >= 100 {
                last_emit.set(now);
                let elapsed = now as f64 / 1000.0;
                update(id, &mut move |s: &mut JobSnapshot| {
                    s.total_bytes = total;
                    s.copied_bytes = copied;
                    s.percent = pct;
                    s.speed_bps = if elapsed > 0.05 { copied as f64 / elapsed } else { 0.0 };
                    s.eta_secs = if pct > 2 && elapsed > 0.2 {
                        elapsed * (100.0 - pct as f64) / pct as f64
                    } else {
                        0.0
                    };
                });
            }
        }
    };
    match job.kind {
        JobKind::Push => {
            // 冲突策略（push）：Skip 时过滤设备上已存在的同名文件；Overwrite 直传
            let files = match job.policy {
                ConflictPolicy::Skip => {
                    let mut keep = Vec::new();
                    for f in &job.files {
                        let remote = format!(
                            "{}/{}",
                            job.remote.trim_end_matches('/'),
                            f.file_name().unwrap_or_default().to_string_lossy()
                        );
                        let exists = adb
                            .shell(
                                &job.serial,
                                &format!("[ -e '{}' ] && echo y || echo n", remote.replace('\'', "'\\''")),
                            )
                            .await
                            .map(|s| s.trim() == "y")
                            .unwrap_or(false);
                        if !exists {
                            keep.push(f.clone());
                        }
                    }
                    if keep.is_empty() {
                        anyhow::bail!("全部文件已存在（策略：跳过）");
                    }
                    keep
                }
                _ => job.files.clone(),
            };
            adb.push(
                &job.serial,
                &files,
                &job.remote,
                cancel,
                make_progress(update.clone()),
            )
            .await?;
        }
        JobKind::Pull => {
            // 目标文件名冲突处理（本地）：先拉到暂存目录，再按策略落位
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
            let staging = local_dir.join(".screenlink_staging");
            std::fs::create_dir_all(&staging)?;
            adb.pull(
                &job.serial,
                &job.remote,
                &staging,
                cancel,
                make_progress(update.clone()),
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
        }
    }
    Ok(())
}
