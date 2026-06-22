use crate::core::models::CoreError;
use crate::platform::paths::CodexPaths;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const TOKEN_USAGE_FILE: &str = "token-usage.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageEntry {
    pub timestamp: u64,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub provider_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelTokenStats {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub request_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenStatsPayload {
    pub today: TokenStatsBucket,
    pub seven_days: TokenStatsBucket,
    pub thirty_days: TokenStatsBucket,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenStatsBucket {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub request_count: u64,
    pub models: Vec<ModelTokenStats>,
}

pub fn record_token_usage(
    paths: &CodexPaths,
    model: &str,
    usage: Option<&Value>,
    provider_id: &str,
) {
    let Some(usage) = usage else {
        return;
    };
    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input_tokens + output_tokens);

    if total_tokens == 0 {
        return;
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let entry = TokenUsageEntry {
        timestamp,
        model: model.to_string(),
        input_tokens,
        output_tokens,
        total_tokens,
        provider_id: provider_id.to_string(),
    };

    let path = paths.codexmate_dir.join(TOKEN_USAGE_FILE);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = serde_json::to_string(&entry).unwrap_or_default();
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

pub fn load_token_stats(paths: &CodexPaths) -> Result<TokenStatsPayload, CoreError> {
    let path = paths.codexmate_dir.join(TOKEN_USAGE_FILE);
    if !path.exists() {
        return Ok(TokenStatsPayload::default());
    }
    let content = std::fs::read_to_string(&path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let one_day_secs: u64 = 86400;
    let today_cutoff = now.saturating_sub(one_day_secs);
    let seven_day_cutoff = now.saturating_sub(one_day_secs * 7);
    let thirty_day_cutoff = now.saturating_sub(one_day_secs * 30);

    let mut today = TokenStatsBucket::default();
    let mut seven_days = TokenStatsBucket::default();
    let mut thirty_days = TokenStatsBucket::default();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<TokenUsageEntry>(line) else {
            continue;
        };
        if entry.timestamp >= thirty_day_cutoff {
            aggregate_into(&mut thirty_days, &entry);
            if entry.timestamp >= seven_day_cutoff {
                aggregate_into(&mut seven_days, &entry);
                if entry.timestamp >= today_cutoff {
                    aggregate_into(&mut today, &entry);
                }
            }
        }
    }

    finalize_bucket(&mut today);
    finalize_bucket(&mut seven_days);
    finalize_bucket(&mut thirty_days);

    Ok(TokenStatsPayload {
        today,
        seven_days,
        thirty_days,
    })
}

fn aggregate_into(bucket: &mut TokenStatsBucket, entry: &TokenUsageEntry) {
    bucket.total_tokens += entry.total_tokens;
    bucket.input_tokens += entry.input_tokens;
    bucket.output_tokens += entry.output_tokens;
    bucket.request_count += 1;
    if let Some(model_stats) = bucket.models.iter_mut().find(|m| m.model == entry.model) {
        model_stats.input_tokens += entry.input_tokens;
        model_stats.output_tokens += entry.output_tokens;
        model_stats.total_tokens += entry.total_tokens;
        model_stats.request_count += 1;
    } else {
        bucket.models.push(ModelTokenStats {
            model: entry.model.clone(),
            input_tokens: entry.input_tokens,
            output_tokens: entry.output_tokens,
            total_tokens: entry.total_tokens,
            request_count: 1,
        });
    }
}

fn finalize_bucket(bucket: &mut TokenStatsBucket) {
    bucket.models.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn paths_for(root: &std::path::Path) -> CodexPaths {
        let codex_home = root.to_path_buf();
        CodexPaths {
            codex_home: codex_home.clone(),
            auth_path: codex_home.join("auth.json"),
            config_path: codex_home.join("config.toml"),
            session_index_path: codex_home.join("session_index.jsonl"),
            codex_state_db_path: codex_home.join("state_5.sqlite"),
            sessions_dir: codex_home.join("sessions"),
            archived_sessions_dir: codex_home.join("archived_sessions"),
            skills_dir: codex_home.join("skills"),
            accounts_dir: codex_home.join("accounts"),
            registry_path: codex_home.join("registry.json"),
            snapshots_dir: codex_home.join("snapshots"),
            auth_backups_dir: codex_home.join("backups"),
            registry_backups_dir: codex_home.join("registry-backups"),
            auto_switch_log_path: codex_home.join("auto-switch.log"),
            codexmate_dir: codex_home.join("codexmate"),
            skill_backups_dir: codex_home.join("skill-backups"),
            quota_history_path: codex_home.join("quota-history.jsonl"),
            quota_store_path: codex_home.join("quota-store.json"),
            settings_path: codex_home.join("settings.json"),
            bootstrap_cache_path: codex_home.join("bootstrap-cache.json"),
            auto_switch_pending_path: codex_home.join("auto-switch-pending.json"),
            auto_switch_snooze_path: codex_home.join("auto-switch-snooze.json"),
            voice_workspace_path: codex_home.join("voice-workspace.json"),
            voice_runtime_path: codex_home.join("voice-runtime.json"),
            launch_agent_path: codex_home.join("launch-agent.plist"),
            global_agents_path: codex_home.join("AGENTS.md"),
            custom_instructions_dir: codex_home.join("custom-instructions"),
            custom_instruction_history_dir: codex_home.join("custom-instructions/history"),
            token_usage_path: codex_home.join("codexmate/token-usage.jsonl"),
        }
    }

    #[test]
    fn record_and_load_token_stats() {
        let root = std::env::temp_dir().join(format!("token-usage-record-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        let _ = fs::create_dir_all(&root);
        let paths = paths_for(&root);

        let usage = serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "total_tokens": 150
        });
        record_token_usage(&paths, "glm-5.2", Some(&usage), "dashscope");
        record_token_usage(&paths, "gpt-5.5", Some(&usage), "openai");

        let stats = load_token_stats(&paths).unwrap();
        assert_eq!(stats.today.request_count, 2);
        assert_eq!(stats.today.total_tokens, 300);
        assert_eq!(stats.seven_days.request_count, 2);
        assert_eq!(stats.thirty_days.request_count, 2);
        assert_eq!(stats.today.models.len(), 2);
    }

    #[test]
    fn record_with_zero_tokens_is_skipped() {
        let root = std::env::temp_dir().join(format!("token-usage-record-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        let _ = fs::create_dir_all(&root);
        let paths = paths_for(&root);

        let usage = serde_json::json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0
        });
        record_token_usage(&paths, "glm-5.2", Some(&usage), "dashscope");

        let stats = load_token_stats(&paths).unwrap();
        assert_eq!(stats.today.request_count, 0);
    }

    #[test]
    fn load_stats_when_file_missing() {
        let root = std::env::temp_dir().join(format!("token-usage-record-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        let _ = fs::create_dir_all(&root);
        let paths = paths_for(&root);
        let stats = load_token_stats(&paths).unwrap();
        assert_eq!(stats.today.request_count, 0);
    }
}
