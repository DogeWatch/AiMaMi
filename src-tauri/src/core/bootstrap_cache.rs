use crate::core::analytics::UsageAnalyticsPayload;
use crate::core::models::{CoreError, CoreSnapshotPayload, McpServerListPayload, SkillListPayload};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapStatePayload {
    pub written_at: Option<i64>,
    pub snapshot_progressive: Option<CoreSnapshotPayload>,
    pub usage_analytics: Option<UsageAnalyticsPayload>,
    pub mcp_servers: Option<McpServerListPayload>,
    pub installed_skills: Option<SkillListPayload>,
}

pub fn load(path: &Path) -> BootstrapStatePayload {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

pub fn update<F>(path: &Path, mut apply: F) -> Result<(), CoreError>
where
    F: FnMut(&mut BootstrapStatePayload),
{
    let mut state = load(path);
    apply(&mut state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(&state)?;
    std::fs::write(path, data)?;
    Ok(())
}
