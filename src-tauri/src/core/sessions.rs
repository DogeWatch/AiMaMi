use crate::core::auth::current_timestamp;
use crate::core::models::CoreError;
use crate::platform::paths::CodexPaths;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecordPayload {
    pub id: String,
    pub thread_name: String,
    pub project_path: Option<String>,
    pub project_name: Option<String>,
    pub model_provider: Option<String>,
    pub parent_session_id: Option<String>,
    pub updated_at: i64,
    pub created_at: Option<i64>,
    pub file_size: i64,
    pub is_conversation_thread: bool,
    pub project_path_missing: bool,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionProviderBucketPayload {
    pub model_provider: String,
    pub count: i32,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionProviderMigrationPreviewPayload {
    pub source_model_provider: String,
    pub target_model_provider: String,
    pub file_session_count: i32,
    pub state_thread_count: Option<i32>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionProviderMigrationLedgerPayload {
    pub path: String,
    pub created_at: i64,
    pub source_model_provider: String,
    pub target_model_provider: String,
    pub file_sessions: Vec<SessionProviderMigrationFilePayload>,
    pub state_threads: Vec<SessionProviderMigrationThreadPayload>,
    pub state_index_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionProviderMigrationFilePayload {
    pub id: String,
    pub path: String,
    pub backup_path: String,
    pub original_model_provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionProviderMigrationThreadPayload {
    pub id: String,
    pub rollout_path: String,
    pub original_model_provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionProviderAutoSyncLedgerPayload {
    pub path: String,
    pub created_at: i64,
    pub target_model_provider: String,
    pub files: Vec<SessionProviderAutoSyncFilePayload>,
    pub state_threads: Vec<SessionProviderMigrationThreadPayload>,
    pub state_index_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionProviderAutoSyncFilePayload {
    pub id: String,
    pub path: String,
    pub original_model_provider: String,
    pub target_model_provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionsListPayload {
    pub items: Vec<SessionRecordPayload>,
    pub total: i32,
    pub active_model_provider: String,
    pub provider_buckets: Vec<SessionProviderBucketPayload>,
    pub state_provider_buckets: Vec<SessionProviderBucketPayload>,
    pub state_index_available: bool,
    pub state_index_error: Option<String>,
    pub migration_preview: SessionProviderMigrationPreviewPayload,
    pub source_path: String,
    pub archive_path: String,
    pub last_scan_at: i64,
}

const SESSION_PROVIDER_MIGRATION_SOURCE: &str = "openai";
const SESSION_PROVIDER_MIGRATION_DIR: &str = "session-provider-migration-ledgers";
const SESSION_PROVIDER_AUTO_SYNC_DIR: &str = "session-provider-auto-sync-ledgers";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionsDeletePayload {
    pub requested_ids: Vec<String>,
    pub deleted_ids: Vec<String>,
    pub skipped_ids: Vec<String>,
    pub deleted_count: i32,
    pub source_path: String,
    pub archive_path: String,
}

pub fn load_sessions(paths: &CodexPaths) -> Result<SessionsListPayload, CoreError> {
    let mut items = Vec::new();

    if paths.sessions_dir.exists() {
        for path in collect_session_files(&paths.sessions_dir)? {
            if let Some(record) = session_record_from_path(&path)? {
                items.push(record);
            }
        }
    }

    items.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    let active_model_provider = active_model_provider(paths);
    let provider_buckets = provider_buckets(&items, &active_model_provider);
    let state_index = load_state_provider_buckets(paths, &active_model_provider);
    let migration_preview =
        migration_preview(&active_model_provider, &provider_buckets, &state_index.buckets);

    Ok(SessionsListPayload {
        total: items.len() as i32,
        items,
        active_model_provider,
        provider_buckets,
        state_provider_buckets: state_index.buckets,
        state_index_available: state_index.available,
        state_index_error: state_index.error,
        migration_preview,
        source_path: paths.sessions_dir.display().to_string(),
        archive_path: paths.archived_sessions_dir.display().to_string(),
        last_scan_at: current_timestamp(),
    })
}

pub fn prepare_session_provider_migration(
    paths: &CodexPaths,
) -> Result<SessionProviderMigrationLedgerPayload, CoreError> {
    let target_model_provider = active_model_provider(paths);
    if target_model_provider == SESSION_PROVIDER_MIGRATION_SOURCE {
        return Err(CoreError::InvalidData(
            "Active model provider is already openai".to_string(),
        ));
    }
    prepare_session_provider_migration_for_source(
        paths,
        SESSION_PROVIDER_MIGRATION_SOURCE,
        &target_model_provider,
    )
}

pub fn migrate_session_provider_buckets_to_active(
    paths: &CodexPaths,
    source_model_providers: &[&str],
) -> Result<Vec<SessionProviderMigrationLedgerPayload>, CoreError> {
    let target_model_provider = active_model_provider(paths);
    let mut ledgers = Vec::new();

    for source_model_provider in source_model_providers {
        let source_model_provider = source_model_provider.trim();
        if source_model_provider.is_empty() || source_model_provider == target_model_provider {
            continue;
        }

        let ledger = prepare_session_provider_migration_for_source(
            paths,
            source_model_provider,
            &target_model_provider,
        )?;
        if ledger.file_sessions.is_empty() && ledger.state_threads.is_empty() {
            let _ = fs::remove_file(&ledger.path);
            continue;
        }

        for session in &ledger.file_sessions {
            backup_session_file(Path::new(&session.path), Path::new(&session.backup_path))?;
            rewrite_session_file_model_provider(
                Path::new(&session.path),
                source_model_provider,
                &target_model_provider,
            )?;
        }

        if !ledger.state_threads.is_empty() {
            let _ = update_state_threads_model_provider(
                paths,
                source_model_provider,
                &target_model_provider,
            );
        }

        ledgers.push(ledger);
    }

    Ok(ledgers)
}

pub fn migrate_all_session_provider_buckets_to_active(
    paths: &CodexPaths,
) -> Result<Vec<SessionProviderMigrationLedgerPayload>, CoreError> {
    let target_model_provider = active_model_provider(paths);
    let sessions = load_sessions(paths)?;
    let mut sources = BTreeSet::<String>::new();
    for bucket in sessions
        .provider_buckets
        .iter()
        .chain(sessions.state_provider_buckets.iter())
    {
        if bucket.model_provider != target_model_provider && bucket.model_provider != "unknown" {
            sources.insert(bucket.model_provider.clone());
        }
    }
    let source_refs = sources.iter().map(String::as_str).collect::<Vec<_>>();
    migrate_session_provider_buckets_to_active(paths, &source_refs)
}

pub fn auto_sync_session_provider_buckets_to_active(
    paths: &CodexPaths,
) -> Result<Option<SessionProviderAutoSyncLedgerPayload>, CoreError> {
    let target_model_provider = active_model_provider(paths);
    let mut file_changes = BTreeMap::<PathBuf, SessionProviderAutoSyncFilePayload>::new();

    for session in load_sessions(paths)?.items {
        let Some(source_model_provider) = syncable_source_provider(
            session.model_provider.as_deref(),
            &target_model_provider,
        ) else {
            continue;
        };
        file_changes.insert(
            PathBuf::from(&session.path),
            SessionProviderAutoSyncFilePayload {
                id: session.id,
                path: session.path,
                original_model_provider: source_model_provider.to_string(),
                target_model_provider: target_model_provider.clone(),
            },
        );
    }

    let state_threads = load_state_threads_except_provider(paths, &target_model_provider);
    let (state_threads, state_index_error) = match state_threads {
        Ok(threads) => (threads, None),
        Err(error) => (Vec::new(), Some(error)),
    };

    for thread in &state_threads {
        let Some(path) = rollout_path_in_codex_home(paths, &thread.rollout_path) else {
            continue;
        };
        if file_changes.contains_key(&path) {
            continue;
        }
        let meta = session_meta_from_path(&path)?;
        let source_model_provider = meta
            .model_provider
            .as_deref()
            .or(Some(thread.original_model_provider.as_str()));
        let Some(source_model_provider) =
            syncable_source_provider(source_model_provider, &target_model_provider)
        else {
            continue;
        };
        let id = meta
            .id
            .unwrap_or_else(|| thread.id.clone())
            .trim()
            .to_string();
        file_changes.insert(
            path.clone(),
            SessionProviderAutoSyncFilePayload {
                id,
                path: path.display().to_string(),
                original_model_provider: source_model_provider.to_string(),
                target_model_provider: target_model_provider.clone(),
            },
        );
    }

    if file_changes.is_empty() && state_threads.is_empty() {
        return Ok(None);
    }

    for file in file_changes.values() {
        rewrite_session_file_model_provider(
            Path::new(&file.path),
            &file.original_model_provider,
            &target_model_provider,
        )?;
    }

    let mut state_update_error = state_index_error;
    let mut source_model_providers = BTreeSet::<String>::new();
    for file in file_changes.values() {
        source_model_providers.insert(file.original_model_provider.clone());
    }
    for thread in &state_threads {
        source_model_providers.insert(thread.original_model_provider.clone());
    }
    for source_model_provider in source_model_providers {
        if source_model_provider != target_model_provider {
            if let Err(error) =
                update_state_threads_model_provider(paths, &source_model_provider, &target_model_provider)
            {
                state_update_error = Some(error);
            }
        }
    }

    let ledger_dir = paths.codexmate_dir.join(SESSION_PROVIDER_AUTO_SYNC_DIR);
    fs::create_dir_all(&ledger_dir)?;
    let created_at = current_timestamp();
    let path = unique_archive_path(&ledger_dir.join(format!(
        "to-{}-{}.json",
        target_model_provider, created_at
    )));
    let ledger = SessionProviderAutoSyncLedgerPayload {
        path: path.display().to_string(),
        created_at,
        target_model_provider,
        files: file_changes.into_values().collect(),
        state_threads,
        state_index_error: state_update_error,
    };
    fs::write(&path, serde_json::to_string_pretty(&ledger)?)?;
    Ok(Some(ledger))
}

fn syncable_source_provider<'a>(
    source_model_provider: Option<&'a str>,
    target_model_provider: &str,
) -> Option<&'a str> {
    let source_model_provider = source_model_provider?.trim();
    if source_model_provider.is_empty()
        || source_model_provider == "unknown"
        || source_model_provider == target_model_provider
    {
        None
    } else {
        Some(source_model_provider)
    }
}

fn prepare_session_provider_migration_for_source(
    paths: &CodexPaths,
    source_model_provider: &str,
    target_model_provider: &str,
) -> Result<SessionProviderMigrationLedgerPayload, CoreError> {
    let created_at = current_timestamp();
    let ledger_dir = paths
        .codexmate_dir
        .join(SESSION_PROVIDER_MIGRATION_DIR);
    fs::create_dir_all(&ledger_dir)?;
    let path = unique_archive_path(&ledger_dir.join(format!(
        "{}-to-{}-{}.json",
        source_model_provider, target_model_provider, created_at
    )));
    let sessions = load_sessions(paths)?;
    let file_sessions = sessions
        .items
        .into_iter()
        .filter(|session| session.model_provider.as_deref() == Some(source_model_provider))
        .map(|session| SessionProviderMigrationFilePayload {
            id: session.id,
            backup_path: session_backup_path(paths, &path, Path::new(&session.path))
                .display()
                .to_string(),
            path: session.path,
            original_model_provider: source_model_provider.to_string(),
        })
        .collect::<Vec<_>>();
    let state_threads = load_state_threads_for_provider(paths, source_model_provider);
    let (state_threads, state_index_error) = match state_threads {
        Ok(threads) => (threads, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let ledger = SessionProviderMigrationLedgerPayload {
        path: path.display().to_string(),
        created_at,
        source_model_provider: source_model_provider.to_string(),
        target_model_provider: target_model_provider.to_string(),
        file_sessions,
        state_threads,
        state_index_error,
    };
    fs::write(&path, serde_json::to_string_pretty(&ledger)?)?;
    Ok(ledger)
}

fn session_backup_path(paths: &CodexPaths, ledger_path: &Path, session_path: &Path) -> PathBuf {
    let ledger_stem = ledger_path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "session-provider-migration".to_string());
    let relative = session_path
        .strip_prefix(&paths.sessions_dir)
        .ok()
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| {
            session_path
                .file_name()
                .map(|file_name| PathBuf::from(file_name.to_os_string()))
        })
        .unwrap_or_else(|| PathBuf::from("session.jsonl"));
    paths
        .codexmate_dir
        .join("session-provider-migration-backups")
        .join(ledger_stem)
        .join(relative)
}

fn backup_session_file(path: &Path, backup_path: &Path) -> Result<(), CoreError> {
    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(path, backup_path)?;
    Ok(())
}

fn rewrite_session_file_model_provider(
    path: &Path,
    source_model_provider: &str,
    target_model_provider: &str,
) -> Result<(), CoreError> {
    let raw = fs::read_to_string(path)?;
    let mut changed = false;
    let mut output = Vec::new();

    for line in raw.lines() {
        if !changed {
            if let Some(rewritten) =
                rewrite_session_meta_provider_line(line, source_model_provider, target_model_provider)?
            {
                output.push(rewritten);
                changed = true;
                continue;
            }
        }
        output.push(line.to_string());
    }

    if !changed {
        return Ok(());
    }

    let mut next = output.join("\n");
    if raw.ends_with('\n') {
        next.push('\n');
    }
    let temp_path = unique_archive_path(&path.with_extension("tmp"));
    fs::write(&temp_path, next)?;
    fs::rename(temp_path, path)?;
    Ok(())
}

fn rewrite_session_meta_provider_line(
    line: &str,
    source_model_provider: &str,
    target_model_provider: &str,
) -> Result<Option<String>, CoreError> {
    let mut value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Ok(None);
    }
    let top_level_matches =
        value.get("model_provider").and_then(Value::as_str) == Some(source_model_provider);
    let payload_matches = value
        .get("payload")
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("model_provider"))
        .and_then(Value::as_str)
        == Some(source_model_provider);
    if !top_level_matches && !payload_matches {
        return Ok(None);
    }

    if top_level_matches {
        value["model_provider"] = Value::String(target_model_provider.to_string());
    }
    if payload_matches {
        let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut) else {
            return Ok(None);
        };
        payload.insert(
            "model_provider".to_string(),
            Value::String(target_model_provider.to_string()),
        );
    }
    if !value.get("model_provider").is_some() && payload_matches {
        value["model_provider"] = Value::String(target_model_provider.to_string());
    }

    Ok(Some(serde_json::to_string(&value)?))
}

fn update_state_threads_model_provider(
    paths: &CodexPaths,
    source_model_provider: &str,
    target_model_provider: &str,
) -> Result<(), String> {
    if !paths.codex_state_db_path.exists() {
        return Ok(());
    }
    let sql = format!(
        "update threads set model_provider = '{}' where model_provider = '{}';",
        sql_quote(target_model_provider),
        sql_quote(source_model_provider)
    );
    let output = Command::new("sqlite3")
        .arg(&paths.codex_state_db_path)
        .arg(sql)
        .output()
        .map_err(|error| format!("sqlite3 unavailable: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn load_state_threads_for_provider(
    paths: &CodexPaths,
    model_provider: &str,
) -> Result<Vec<SessionProviderMigrationThreadPayload>, String> {
    if !paths.codex_state_db_path.exists() {
        return Err("state_5.sqlite not found".to_string());
    }
    let output = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-separator")
        .arg("\t")
        .arg(&paths.codex_state_db_path)
        .arg(format!(
            "select id, rollout_path, model_provider from threads where model_provider = '{}' order by updated_at desc, id desc;",
            sql_quote(model_provider)
        ))
        .output()
        .map_err(|error| format!("sqlite3 unavailable: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parse_state_thread_rows(&String::from_utf8_lossy(&output.stdout))
}

fn load_state_threads_except_provider(
    paths: &CodexPaths,
    model_provider: &str,
) -> Result<Vec<SessionProviderMigrationThreadPayload>, String> {
    if !paths.codex_state_db_path.exists() {
        return Err("state_5.sqlite not found".to_string());
    }
    let output = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-separator")
        .arg("\t")
        .arg(&paths.codex_state_db_path)
        .arg(format!(
            "select id, rollout_path, model_provider from threads \
             where coalesce(nullif(trim(model_provider), ''), 'unknown') not in ('{}', 'unknown') \
             order by updated_at desc, id desc;",
            sql_quote(model_provider)
        ))
        .output()
        .map_err(|error| format!("sqlite3 unavailable: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parse_state_thread_rows(&String::from_utf8_lossy(&output.stdout))
}

fn rollout_path_in_codex_home(paths: &CodexPaths, rollout_path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(rollout_path.trim());
    if !path.exists() {
        return None;
    }
    let codex_home = paths.codex_home.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    path.starts_with(&codex_home).then_some(path)
}

fn parse_state_thread_rows(
    output: &str,
) -> Result<Vec<SessionProviderMigrationThreadPayload>, String> {
    let mut threads = Vec::new();
    for line in output.lines() {
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() != 3 {
            return Err(format!("Invalid state thread row: {line}"));
        }
        threads.push(SessionProviderMigrationThreadPayload {
            id: columns[0].to_string(),
            rollout_path: columns[1].to_string(),
            original_model_provider: columns[2].to_string(),
        });
    }
    Ok(threads)
}

fn sql_quote(value: &str) -> String {
    value.replace('\'', "''")
}

fn migration_preview(
    active_model_provider: &str,
    provider_buckets: &[SessionProviderBucketPayload],
    state_provider_buckets: &[SessionProviderBucketPayload],
) -> SessionProviderMigrationPreviewPayload {
    let source = "openai";
    let file_session_count = bucket_count(provider_buckets, source);
    let state_thread_count = if state_provider_buckets.is_empty() {
        None
    } else {
        Some(bucket_count(state_provider_buckets, source))
    };
    SessionProviderMigrationPreviewPayload {
        source_model_provider: source.to_string(),
        target_model_provider: active_model_provider.to_string(),
        file_session_count,
        state_thread_count,
        required: active_model_provider != source
            && (file_session_count > 0 || state_thread_count.unwrap_or(0) > 0),
    }
}

fn bucket_count(buckets: &[SessionProviderBucketPayload], model_provider: &str) -> i32 {
    buckets
        .iter()
        .find(|bucket| bucket.model_provider == model_provider)
        .map(|bucket| bucket.count)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default)]
struct StateProviderBuckets {
    buckets: Vec<SessionProviderBucketPayload>,
    available: bool,
    error: Option<String>,
}

fn load_state_provider_buckets(
    paths: &CodexPaths,
    active_model_provider: &str,
) -> StateProviderBuckets {
    if !paths.codex_state_db_path.exists() {
        return StateProviderBuckets {
            error: Some("state_5.sqlite not found".to_string()),
            ..StateProviderBuckets::default()
        };
    }

    let output = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-separator")
        .arg("\t")
        .arg(&paths.codex_state_db_path)
        .arg(
            "select coalesce(nullif(trim(model_provider), ''), 'unknown'), count(*) \
             from threads group by 1 order by 1;",
        )
        .output();

    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return StateProviderBuckets {
                error: Some(format!("sqlite3 unavailable: {error}")),
                ..StateProviderBuckets::default()
            };
        }
    };
    if !output.status.success() {
        return StateProviderBuckets {
            error: Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
            ..StateProviderBuckets::default()
        };
    }
    match parse_state_provider_bucket_rows(
        &String::from_utf8_lossy(&output.stdout),
        active_model_provider,
    ) {
        Ok(buckets) => StateProviderBuckets {
            buckets,
            available: true,
            error: None,
        },
        Err(error) => StateProviderBuckets {
            error: Some(error),
            ..StateProviderBuckets::default()
        },
    }
}

fn parse_state_provider_bucket_rows(
    output: &str,
    active_model_provider: &str,
) -> Result<Vec<SessionProviderBucketPayload>, String> {
    let mut buckets = Vec::new();
    for line in output.lines() {
        let Some((provider, count)) = line.split_once('\t') else {
            return Err(format!("Invalid state index row: {line}"));
        };
        let model_provider = provider.trim();
        let model_provider = if model_provider.is_empty() {
            "unknown"
        } else {
            model_provider
        };
        let count = count
            .trim()
            .parse::<i32>()
            .map_err(|error| format!("Invalid state index count: {error}"))?;
        buckets.push(SessionProviderBucketPayload {
            model_provider: model_provider.to_string(),
            count,
            active: model_provider == active_model_provider,
        });
    }
    buckets.sort_by(|left, right| {
        right
            .active
            .cmp(&left.active)
            .then_with(|| left.model_provider.cmp(&right.model_provider))
    });
    Ok(buckets)
}

fn active_model_provider(paths: &CodexPaths) -> String {
    let Ok(raw) = fs::read_to_string(&paths.config_path) else {
        return "openai".to_string();
    };
    let Ok(value) = raw.parse::<toml::Value>() else {
        return "openai".to_string();
    };
    value
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("openai")
        .to_string()
}

fn provider_buckets(
    items: &[SessionRecordPayload],
    active_model_provider: &str,
) -> Vec<SessionProviderBucketPayload> {
    let mut counts = BTreeMap::<String, i32>::new();
    for item in items {
        let provider = item
            .model_provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        *counts.entry(provider.to_string()).or_default() += 1;
    }

    let mut buckets = counts
        .into_iter()
        .map(|(model_provider, count)| SessionProviderBucketPayload {
            active: model_provider == active_model_provider,
            model_provider,
            count,
        })
        .collect::<Vec<_>>();
    buckets.sort_by(|left, right| {
        right
            .active
            .cmp(&left.active)
            .then_with(|| left.model_provider.cmp(&right.model_provider))
    });
    buckets
}

pub fn delete_sessions(
    paths: &CodexPaths,
    ids: Vec<String>,
) -> Result<SessionsDeletePayload, CoreError> {
    fs::create_dir_all(&paths.archived_sessions_dir)?;

    let mut deleted_ids = Vec::new();
    let mut skipped_ids = Vec::new();

    for id in &ids {
        let Some(source_path) = find_session_file(&paths.sessions_dir, id)? else {
            skipped_ids.push(id.clone());
            continue;
        };
        let file_name = source_path
            .file_name()
            .ok_or_else(|| CoreError::InvalidData(format!("Invalid session file: {id}")))?;
        let target_path = unique_archive_path(&paths.archived_sessions_dir.join(file_name));
        fs::rename(&source_path, target_path)?;
        deleted_ids.push(id.clone());
    }

    Ok(SessionsDeletePayload {
        requested_ids: ids,
        deleted_count: deleted_ids.len() as i32,
        deleted_ids,
        skipped_ids,
        source_path: paths.sessions_dir.display().to_string(),
        archive_path: paths.archived_sessions_dir.display().to_string(),
    })
}

fn session_record_from_path(path: &Path) -> Result<Option<SessionRecordPayload>, CoreError> {
    let meta = session_meta_from_path(path)?;
    let Some(id) = meta.id.or_else(|| session_id_from_path(path)) else {
        return Ok(None);
    };
    let metadata = fs::metadata(path)?;
    let updated_at = metadata
        .modified()
        .ok()
        .and_then(system_time_to_unix_seconds)
        .unwrap_or(0);
    let created_at = metadata.created().ok().and_then(system_time_to_unix_seconds);

    Ok(Some(SessionRecordPayload {
        id: id.clone(),
        thread_name: id,
        project_path: meta.project_path.clone(),
        project_name: meta.project_name,
        model_provider: meta.model_provider,
        parent_session_id: None,
        updated_at,
        created_at: meta.created_at.or(created_at),
        file_size: metadata.len().min(i64::MAX as u64) as i64,
        is_conversation_thread: false,
        project_path_missing: meta
            .project_path
            .as_deref()
            .is_some_and(|path| !Path::new(path).exists()),
        path: path.display().to_string(),
    }))
}

fn find_session_file(sessions_dir: &Path, id: &str) -> Result<Option<PathBuf>, CoreError> {
    if id.contains('/') || id.contains('\\') || id.trim().is_empty() {
        return Ok(None);
    }

    for path in collect_session_files(sessions_dir)? {
        if session_record_from_path(&path)?
            .as_ref()
            .map(|record| record.id.as_str())
            == Some(id)
        {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

fn collect_session_files(root: &Path) -> Result<Vec<PathBuf>, CoreError> {
    let mut files = Vec::new();
    collect_session_files_inner(root, &mut files)?;
    Ok(files)
}

fn collect_session_files_inner(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), CoreError> {
    if !root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_session_files_inner(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn session_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Default)]
struct SessionMeta {
    id: Option<String>,
    project_path: Option<String>,
    project_name: Option<String>,
    model_provider: Option<String>,
    created_at: Option<i64>,
}

fn session_meta_from_path(path: &Path) -> Result<SessionMeta, CoreError> {
    let raw = fs::read_to_string(path)?;

    for line in raw.lines().take(20) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }

        let payload = value.get("payload").unwrap_or(&Value::Null);
        let project_path = value
            .get("cwd")
            .or_else(|| payload.get("cwd"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        return Ok(SessionMeta {
            id: value
                .get("id")
                .or_else(|| payload.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            project_name: project_path.as_deref().and_then(project_name_from_path),
            model_provider: value
                .get("model_provider")
                .or_else(|| payload.get("model_provider"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            project_path,
            created_at: value
                .get("timestamp")
                .or_else(|| payload.get("timestamp"))
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_seconds),
        });
    }

    Ok(SessionMeta::default())
}

fn project_name_from_path(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.trim().is_empty())
}

fn parse_rfc3339_seconds(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp())
}

fn unique_archive_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "session".to_string());
    let extension = path.extension().map(|value| value.to_string_lossy().to_string());
    let suffix = current_timestamp();

    for index in 0..1000 {
        let suffix = if index == 0 {
            suffix.to_string()
        } else {
            format!("{suffix}-{index}")
        };
        let file_name = match extension.as_deref() {
            Some(extension) if !extension.is_empty() => format!("{stem}-{suffix}.{extension}"),
            _ => format!("{stem}-{suffix}"),
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    parent.join(format!("{stem}-{suffix}-fallback"))
}

fn system_time_to_unix_seconds(value: std::time::SystemTime) -> Option<i64> {
    value
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(label: &str) -> (CodexPaths, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "aimami-sessions-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let paths = CodexPaths::from_home(root.clone());
        fs::create_dir_all(&paths.sessions_dir).unwrap();
        (paths, root)
    }

    #[test]
    fn load_sessions_reads_real_files_and_sorts_newest_first() {
        let (paths, root) = paths("load");
        let older = paths.sessions_dir.join("older.jsonl");
        let newer = paths.sessions_dir.join("newer.jsonl");
        fs::write(&older, "{}\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&newer, "{}\n{}\n").unwrap();
        fs::create_dir(paths.sessions_dir.join("nested")).unwrap();

        let payload = load_sessions(&paths).unwrap();

        assert_eq!(payload.total, 2);
        assert_eq!(payload.items[0].id, "newer");
        assert_eq!(payload.items[1].id, "older");
        assert_eq!(payload.items[0].file_size, 6);
        assert_eq!(payload.source_path, paths.sessions_dir.display().to_string());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_sessions_recurses_rollout_tree_and_reads_session_meta() {
        let (paths, root) = paths("meta");
        let nested = paths.sessions_dir.join("2026/06/18");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("rollout-2026-06-18T00-00-00-abc.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"abc","cwd":"/tmp/project-a","model_provider":"openai"}}"#,
        )
        .unwrap();

        let payload = load_sessions(&paths).unwrap();

        assert_eq!(payload.total, 1);
        assert_eq!(payload.items[0].id, "abc");
        assert_eq!(payload.items[0].project_path.as_deref(), Some("/tmp/project-a"));
        assert_eq!(payload.items[0].project_name.as_deref(), Some("project-a"));
        assert_eq!(payload.items[0].model_provider.as_deref(), Some("openai"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_sessions_reads_codex_desktop_top_level_session_meta() {
        let (paths, root) = paths("top-level-meta");
        let nested = paths.sessions_dir.join("2026/06/19");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("rollout-top-level.jsonl"),
            r#"{"id":"top-level-id","type":"session_meta","cwd":"/tmp/project-top","model_provider":"custom","timestamp":"2026-06-19T01:02:03Z","payload":{"base_instructions":{"text":"ignored"}}}"#,
        )
        .unwrap();

        let payload = load_sessions(&paths).unwrap();

        assert_eq!(payload.total, 1);
        assert_eq!(payload.items[0].id, "top-level-id");
        assert_eq!(payload.items[0].project_path.as_deref(), Some("/tmp/project-top"));
        assert_eq!(payload.items[0].project_name.as_deref(), Some("project-top"));
        assert_eq!(payload.items[0].model_provider.as_deref(), Some("custom"));
        assert_eq!(payload.items[0].created_at, Some(1_781_830_923));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_sessions_reports_provider_buckets_and_active_provider() {
        let (paths, root) = paths("provider-buckets");
        fs::write(&paths.config_path, r#"model_provider = "aimami""#).unwrap();
        fs::write(
            paths.sessions_dir.join("openai.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"openai-session","model_provider":"openai"}}"#,
        )
        .unwrap();
        fs::write(
            paths.sessions_dir.join("aimami.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"aimami-session","model_provider":"aimami"}}"#,
        )
        .unwrap();
        fs::write(paths.sessions_dir.join("legacy.jsonl"), "{}\n").unwrap();

        let payload = load_sessions(&paths).unwrap();

        assert_eq!(payload.active_model_provider, "aimami");
        assert!(!payload.state_index_available);
        assert_eq!(
            payload.state_index_error.as_deref(),
            Some("state_5.sqlite not found")
        );
        assert_eq!(
            payload.migration_preview,
            SessionProviderMigrationPreviewPayload {
                source_model_provider: "openai".into(),
                target_model_provider: "aimami".into(),
                file_session_count: 1,
                state_thread_count: None,
                required: true,
            }
        );
        assert_eq!(
            payload.provider_buckets,
            vec![
                SessionProviderBucketPayload {
                    model_provider: "aimami".into(),
                    count: 1,
                    active: true,
                },
                SessionProviderBucketPayload {
                    model_provider: "openai".into(),
                    count: 1,
                    active: false,
                },
                SessionProviderBucketPayload {
                    model_provider: "unknown".into(),
                    count: 1,
                    active: false,
                },
            ]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_provider_migration_writes_ledger_for_openai_file_sessions() {
        let (paths, root) = paths("migration-ledger");
        fs::write(&paths.config_path, r#"model_provider = "aimami""#).unwrap();
        fs::write(
            paths.sessions_dir.join("openai.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"openai-session","model_provider":"openai"}}"#,
        )
        .unwrap();
        fs::write(
            paths.sessions_dir.join("aimami.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"aimami-session","model_provider":"aimami"}}"#,
        )
        .unwrap();

        let ledger = prepare_session_provider_migration(&paths).unwrap();

        assert_eq!(ledger.source_model_provider, "openai");
        assert_eq!(ledger.target_model_provider, "aimami");
        assert_eq!(ledger.file_sessions.len(), 1);
        assert_eq!(ledger.file_sessions[0].id, "openai-session");
        assert!(ledger.state_threads.is_empty());
        assert!(Path::new(&ledger.path).exists());
        let raw = fs::read_to_string(&ledger.path).unwrap();
        assert!(raw.contains("\"sourceModelProvider\": \"openai\""));
        assert!(raw.contains("\"targetModelProvider\": \"aimami\""));
        assert!(raw.contains("\"openai-session\""));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_provider_migration_does_not_overwrite_existing_ledger() {
        let (paths, root) = paths("migration-ledger-unique");
        fs::write(&paths.config_path, r#"model_provider = "aimami""#).unwrap();
        fs::write(
            paths.sessions_dir.join("openai.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"openai-session","model_provider":"openai"}}"#,
        )
        .unwrap();

        let first = prepare_session_provider_migration(&paths).unwrap();
        let second = prepare_session_provider_migration(&paths).unwrap();
        let third = prepare_session_provider_migration(&paths).unwrap();

        assert_ne!(first.path, second.path);
        assert_ne!(second.path, third.path);
        assert_ne!(first.path, third.path);
        assert!(Path::new(&first.path).exists());
        assert!(Path::new(&second.path).exists());
        assert!(Path::new(&third.path).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn migrate_provider_bucket_rewrites_top_level_session_meta_provider() {
        let (paths, root) = paths("migration-top-level-meta");
        fs::write(&paths.config_path, r#"model_provider = "custom""#).unwrap();
        let session_path = paths.sessions_dir.join("top-level-openai.jsonl");
        fs::write(
            &session_path,
            r#"{"id":"openai-session","type":"session_meta","cwd":"/tmp/project","model_provider":"openai","timestamp":"2026-06-19T01:02:03Z","payload":{}}"#,
        )
        .unwrap();

        let ledgers = migrate_all_session_provider_buckets_to_active(&paths).unwrap();

        assert_eq!(ledgers.len(), 1);
        let raw = fs::read_to_string(session_path).unwrap();
        assert!(raw.contains(r#""model_provider":"custom""#));
        assert!(raw.contains(r#""id":"openai-session""#));
        let backup_raw = fs::read_to_string(&ledgers[0].file_sessions[0].backup_path).unwrap();
        assert!(backup_raw.contains(r#""model_provider":"openai""#));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_sync_rewrites_sessions_state_and_rollout_without_backups() {
        let (paths, root) = paths("auto-sync");
        fs::write(&paths.config_path, r#"model_provider = "custom""#).unwrap();
        let session_path = paths.sessions_dir.join("openai.jsonl");
        fs::write(
            &session_path,
            r#"{"type":"session_meta","payload":{"id":"openai-session","model_provider":"openai"}}"#,
        )
        .unwrap();
        let rollout_path = paths.codex_home.join("state-rollout.jsonl");
        fs::write(
            &rollout_path,
            r#"{"id":"state-session","type":"session_meta","model_provider":"openai","payload":{}}"#,
        )
        .unwrap();
        let create_sql = format!(
            "create table threads (id text, rollout_path text, model_provider text, updated_at integer);\
             insert into threads values ('state-session', '{}', 'openai', 1);",
            rollout_path.display().to_string().replace('\'', "''")
        );
        let output = Command::new("sqlite3")
            .arg(&paths.codex_state_db_path)
            .arg(create_sql)
            .output()
            .unwrap();
        assert!(output.status.success());

        let ledger = auto_sync_session_provider_buckets_to_active(&paths)
            .unwrap()
            .unwrap();

        assert_eq!(ledger.target_model_provider, "custom");
        assert_eq!(ledger.files.len(), 2);
        assert_eq!(ledger.state_threads.len(), 1);
        assert!(Path::new(&ledger.path).exists());
        assert!(fs::read_to_string(session_path)
            .unwrap()
            .contains(r#""model_provider":"custom""#));
        assert!(fs::read_to_string(rollout_path)
            .unwrap()
            .contains(r#""model_provider":"custom""#));
        let output = Command::new("sqlite3")
            .arg("-readonly")
            .arg(&paths.codex_state_db_path)
            .arg("select model_provider from threads where id = 'state-session';")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "custom");
        assert!(!paths
            .codexmate_dir
            .join("session-provider-migration-backups")
            .exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_state_provider_bucket_rows_marks_active_provider() {
        let buckets = parse_state_provider_bucket_rows("openai\t2\naimami\t1\n\t3\n", "aimami")
            .unwrap();

        assert_eq!(
            buckets,
            vec![
                SessionProviderBucketPayload {
                    model_provider: "aimami".into(),
                    count: 1,
                    active: true,
                },
                SessionProviderBucketPayload {
                    model_provider: "openai".into(),
                    count: 2,
                    active: false,
                },
                SessionProviderBucketPayload {
                    model_provider: "unknown".into(),
                    count: 3,
                    active: false,
                },
            ]
        );
    }

    #[test]
    fn delete_sessions_moves_matching_files_to_archive_and_reports_skips() {
        let (paths, root) = paths("delete");
        fs::write(paths.sessions_dir.join("keep.jsonl"), "{}\n").unwrap();
        fs::write(paths.sessions_dir.join("remove.jsonl"), "{}\n").unwrap();

        let payload = delete_sessions(&paths, vec!["remove".into(), "missing".into()]).unwrap();

        assert_eq!(payload.requested_ids, vec!["remove", "missing"]);
        assert_eq!(payload.deleted_ids, vec!["remove"]);
        assert_eq!(payload.skipped_ids, vec!["missing"]);
        assert_eq!(payload.deleted_count, 1);
        assert!(!paths.sessions_dir.join("remove.jsonl").exists());
        assert!(paths.archived_sessions_dir.join("remove.jsonl").exists());
        assert!(paths.sessions_dir.join("keep.jsonl").exists());

        let _ = fs::remove_dir_all(root);
    }
}
