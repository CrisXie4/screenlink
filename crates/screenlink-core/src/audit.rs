//! AI 操作审计日志：JSONL 追加写，GUI 可 tail 查看。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub ts: String,
    pub client: String,
    pub tool: String,
    pub device: String,
    pub args: serde_json::Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 端到端耗时（ms），用于验证 <60ms/<20ms 指标
    pub took_ms: u128,
}

pub fn now_iso() -> String {
    // 无 chrono 依赖：unix 秒 -> ISO（UTC）
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil from days (Howard Hinnant 算法)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

pub fn append(entry: &AuditEntry) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::auth::AiAccess::audit_path())?;
    writeln!(f, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

/// GUI 审计面板：读最近 n 条（倒序）。
pub fn tail(n: usize) -> Vec<AuditEntry> {
    let content = std::fs::read_to_string(crate::auth::AiAccess::audit_path()).unwrap_or_default();
    let mut out = Vec::new();
    for line in content.lines().rev() {
        if let Ok(e) = serde_json::from_str::<AuditEntry>(line) {
            out.push(e);
            if out.len() >= n {
                break;
            }
        }
    }
    out
}

pub fn clear() -> Result<()> {
    std::fs::write(crate::auth::AiAccess::audit_path(), "")?;
    Ok(())
}
