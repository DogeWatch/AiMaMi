use crate::core::models::{AuthMode, CoreError, PlanType};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct AuthTokens {
    pub id_token: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthFile {
    pub auth_mode: Option<String>,
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub tokens: AuthTokens,
    pub last_refresh: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthSnapshot {
    pub account_key: String,
    pub email: String,
    pub account_name: Option<String>,
    pub workspace_name: Option<String>,
    pub profile_name: Option<String>,
    pub plan: PlanType,
    pub auth_mode: AuthMode,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct ApiRequestContext {
    pub access_token: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwtClaims {
    email: Option<String>,
    name: Option<String>,
    #[serde(alias = "https://api.openai.com/profile_name")]
    profile_name: Option<String>,
    #[serde(alias = "https://api.openai.com/workspace_name")]
    workspace_name: Option<String>,
}

pub fn current_timestamp() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn load_auth_file(path: &Path) -> Result<AuthFile, CoreError> {
    let data = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&data)?)
}

pub fn make_auth_snapshot(auth: &AuthFile, path: &Path) -> Result<AuthSnapshot, CoreError> {
    let claims = auth
        .tokens
        .id_token
        .as_deref()
        .and_then(decode_jwt_claims);
    let auth_mode = parse_auth_mode(auth.auth_mode.as_deref(), auth.openai_api_key.as_deref());
    let account_hint = claims
        .as_ref()
        .and_then(|c| c.email.clone())
        .or_else(|| auth.tokens.account_id.clone())
        .unwrap_or_else(|| path.display().to_string());
    let account_key = stable_account_key(&account_hint);

    Ok(AuthSnapshot {
        account_key,
        email: claims
            .as_ref()
            .and_then(|c| c.email.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        account_name: claims.as_ref().and_then(|c| c.name.clone()),
        workspace_name: claims.as_ref().and_then(|c| c.workspace_name.clone()),
        profile_name: claims.as_ref().and_then(|c| c.profile_name.clone()),
        plan: PlanType::Unknown,
        auth_mode,
        created_at: current_timestamp(),
    })
}

pub fn make_api_request_context(auth: &AuthFile) -> Option<ApiRequestContext> {
    if auth.tokens.access_token.is_none() && auth.openai_api_key.is_none() {
        return None;
    }
    Some(ApiRequestContext {
        access_token: auth.tokens.access_token.clone(),
        api_key: auth.openai_api_key.clone(),
    })
}

fn parse_auth_mode(raw: Option<&str>, api_key: Option<&str>) -> AuthMode {
    match raw.unwrap_or_default().to_ascii_lowercase().as_str() {
        "apikey" | "api_key" => AuthMode::Apikey,
        "chatgpt" => AuthMode::Chatgpt,
        _ if api_key.is_some() => AuthMode::Apikey,
        _ => AuthMode::Chatgpt,
    }
}

fn stable_account_key(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn decode_jwt_claims(token: &str) -> Option<JwtClaims> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}
