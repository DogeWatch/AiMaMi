use crate::core::models::CoreEnvelope;
use crate::core::repository::Repository;
use crate::core::sessions::{
    self, SessionProviderMigrationLedgerPayload, SessionsDeletePayload, SessionsListPayload,
};
use std::sync::Mutex;
use tauri::State;

#[tauri::command]
pub fn load_sessions(
    repo: State<'_, Mutex<Repository>>,
) -> Result<CoreEnvelope<SessionsListPayload>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    sessions::load_sessions(repo.paths())
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn delete_sessions(
    repo: State<'_, Mutex<Repository>>,
    ids: Vec<String>,
) -> Result<CoreEnvelope<SessionsDeletePayload>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    sessions::delete_sessions(repo.paths(), ids)
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn prepare_session_provider_migration(
    repo: State<'_, Mutex<Repository>>,
) -> Result<CoreEnvelope<SessionProviderMigrationLedgerPayload>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    sessions::prepare_session_provider_migration(repo.paths())
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn migrate_session_provider_buckets_to_active(
    repo: State<'_, Mutex<Repository>>,
) -> Result<CoreEnvelope<Vec<SessionProviderMigrationLedgerPayload>>, String> {
    let repo = repo.lock().map_err(|error| error.to_string())?;
    sessions::migrate_all_session_provider_buckets_to_active(repo.paths())
        .map(CoreEnvelope::ok)
        .map_err(|error| error.to_string())
}
