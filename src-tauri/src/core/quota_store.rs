use crate::core::models::{CoreError, RateLimitWindow, UsageSource};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuotaStoreFile {
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub items: Vec<QuotaStoreItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuotaStoreItem {
    pub account_key: String,
    pub captured_at: i64,
    pub usage_source: UsageSource,
    pub primary_window: Option<RateLimitWindow>,
    pub secondary_window: Option<RateLimitWindow>,
    pub token_status: Option<serde_json::Value>,
}

pub fn load_or_default(path: &Path) -> QuotaStoreFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, quota_store: &QuotaStoreFile) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(quota_store)?;
    std::fs::write(path, data)?;
    Ok(())
}

pub fn find_item<'a>(
    quota_store: &'a QuotaStoreFile,
    account_key: &str,
) -> Option<&'a QuotaStoreItem> {
    quota_store
        .items
        .iter()
        .find(|item| item.account_key == account_key)
}

pub fn upsert_item(
    quota_store: &mut QuotaStoreFile,
    item: QuotaStoreItem,
    updated_at: i64,
) -> bool {
    if let Some(existing) = quota_store
        .items
        .iter_mut()
        .find(|existing| existing.account_key == item.account_key)
    {
        if existing == &item {
            return false;
        }
        *existing = item;
    } else {
        quota_store.items.push(item);
    }
    quota_store.updated_at = updated_at;
    true
}
