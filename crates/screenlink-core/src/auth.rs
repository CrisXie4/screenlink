//! AI 访问授权：token + 能力清单，持久化于数据目录。
//!
//! 安全模型（第一优先级）：
//! - GUI 中"启用 AI 访问"生成 32 字节随机 token 并落盘 ai_access.json；
//! - screenlink-mcp 必须携带该 token（配置片段中已生成）；
//! - 用户可随时撤销（删除文件），正在运行的 MCP 会话下一个请求即失效；
//! - 所有 AI 操作写入 audit.log，GUI 可视化审计。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 48 个 hex 字符 = 192bit 随机（uuid v4 底层为系统熵源）。
fn fresh_token() -> String {
    (0..2).map(|_| uuid::Uuid::new_v4().simple().to_string()).collect::<String>()[..48].to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiAccess {
    pub token: String,
    pub enabled_at: String,
    /// 授予的能力（tools 前缀列表）
    pub scopes: Vec<String>,
}

impl AiAccess {
    pub fn path() -> PathBuf {
        crate::util::data_dir().join("ai_access.json")
    }

    pub fn audit_path() -> PathBuf {
        crate::util::data_dir().join("audit.log")
    }

    pub fn load() -> Option<AiAccess> {
        let data = std::fs::read_to_string(Self::path()).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::write(Self::path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// 生成新 token 并启用。
    pub fn enable() -> Result<AiAccess> {
        let token = fresh_token();
        let access = AiAccess {
            token,
            enabled_at: crate::audit::now_iso(),
            scopes: vec![
                "screenlink_screenshot".into(),
                "screenlink_tap".into(),
                "screenlink_swipe".into(),
                "screenlink_type".into(),
                "screenlink_key".into(),
                "screenlink_devices".into(),
                "screenlink_push_file".into(),
                "screenlink_pull_file".into(),
                "screenlink_page_state".into(),
                "resources:screen".into(),
            ],
        };
        access.save()?;
        Ok(access)
    }

    pub fn revoke() -> Result<()> {
        match std::fs::remove_file(Self::path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn verify(token: &str) -> bool {
        match Self::load() {
            Some(a) => !a.token.is_empty() && constant_eq(&a.token, token),
            None => false,
        }
    }

    pub fn enabled() -> bool {
        Self::load().is_some()
    }

    /// MCP 配置片段（粘贴到 Claude Desktop / Cursor）。
    pub fn mcp_snippet(mcp_command: &str) -> String {
        let token = Self::load().map(|a| a.token).unwrap_or_else(|| "<先在 GUI 中启用 AI 访问>".into());
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "screenlink": {
                    "command": mcp_command,
                    "args": ["--device", "auto", "--token", token]
                }
            }
        }))
        .unwrap_or_default()
    }
}

fn constant_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}
