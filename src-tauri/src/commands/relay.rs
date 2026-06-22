use crate::core::models::CoreEnvelope;
use crate::core::relay::{
    self, RelayProviderDraftPayload, RelayProviderPayload, RelayStatePayload, RelayTestPayload,
};
use crate::core::repository::Repository;
use std::sync::Mutex;
use tauri::State;

#[tauri::command]
pub fn load_relay_state(
    repo: State<'_, Mutex<Repository>>,
) -> Result<CoreEnvelope<RelayStatePayload>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    relay::load_relay_state(repo.paths())
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn upsert_relay_provider(
    repo: State<'_, Mutex<Repository>>,
    input: RelayProviderDraftPayload,
) -> Result<CoreEnvelope<RelayProviderPayload>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    relay::upsert_relay_provider(repo.paths(), input)
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn activate_relay_provider(
    repo: State<'_, Mutex<Repository>>,
    provider_id: String,
) -> Result<CoreEnvelope<RelayStatePayload>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    relay::activate_relay_provider(repo.paths(), &provider_id)
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn delete_relay_provider(
    repo: State<'_, Mutex<Repository>>,
    provider_id: String,
) -> Result<CoreEnvelope<RelayStatePayload>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    relay::delete_relay_provider(repo.paths(), &provider_id)
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn test_relay_draft(
    input: RelayProviderDraftPayload,
) -> Result<CoreEnvelope<RelayTestPayload>, String> {
    tauri::async_runtime::spawn_blocking(move || relay::test_relay_draft(input))
        .await
        .map_err(|error| format!("Blocking command task failed: {error}"))?
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn load_token_stats(
    repo: State<'_, Mutex<Repository>>,
) -> Result<CoreEnvelope<crate::core::token_usage::TokenStatsPayload>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    crate::core::token_usage::load_token_stats(repo.paths())
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn load_daily_token_stats(
    repo: State<'_, Mutex<Repository>>,
    days: u32,
) -> Result<CoreEnvelope<Vec<crate::core::token_usage::DailyTokenStats>>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    crate::core::token_usage::load_daily_token_stats(repo.paths(), days)
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}
