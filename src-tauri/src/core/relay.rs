use crate::core::models::{ApiProxyConfigPayload, CoreError};
use crate::core::token_usage;
use crate::platform::paths::CodexPaths;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read as IoRead, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RELAY_REGISTRY_FILE: &str = "relay-providers.json";
const RELAY_CONFIG_BACKUP_FILE: &str = "relay-config-backup.json";
const RELAY_ROUTER_PROVIDER_ID: &str = "custom";
const RELAY_ROUTER_BASE_URL: &str = "http://127.0.0.1:49735/v1";
const RELAY_MANAGED_BLOCK_BEGIN: &str = "# --- AiMaMi Relay Managed Block ---";
const RELAY_MANAGED_BLOCK_END: &str = "# --- End AiMaMi Relay Managed Block ---";
const RELAY_TOP_MANAGED_BLOCK_BEGIN: &str = "# --- AiMaMi Relay Managed Block (top) ---";
const RELAY_TOP_MANAGED_BLOCK_END: &str = "# --- End AiMaMi Relay Managed Block (top) ---";
const RELAY_PROVIDER_MANAGED_BLOCK_BEGIN: &str =
    "# --- AiMaMi Relay Managed Block (providers) ---";
const RELAY_PROVIDER_MANAGED_BLOCK_END: &str =
    "# --- End AiMaMi Relay Managed Block (providers) ---";
const PROVIDER_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
const PROVIDER_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
const PROVIDER_MAX_CONCURRENT_REQUESTS: usize = 3;

static PROVIDER_RATE_SLOTS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
static PROVIDER_CONCURRENCY_LIMITS: OnceLock<
    Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelayProviderDraftPayload {
    pub id: Option<String>,
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub wire_api: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelayProviderPayload {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key_stored: bool,
    pub model: String,
    pub wire_api: String,
    pub active: bool,
    pub models_sample: Vec<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatePayload {
    pub providers: Vec<RelayProviderPayload>,
    pub active_provider_id: Option<String>,
    pub source_path: String,
    pub codex_config_path: String,
    pub diagnostics: RelayDiagnosticsPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelayDiagnosticsPayload {
    pub registry_exists: bool,
    pub config_exists: bool,
    pub managed_block_present: bool,
    pub active_provider_configured: bool,
    pub relay_server_reachable: bool,
    pub issue_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelayTestPayload {
    pub ok: bool,
    pub status_code: Option<u16>,
    pub models: Vec<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelayHttpResponse {
    status_code: u16,
    content_type: String,
    body: Vec<u8>,
}

struct RelayHttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RelaySettingsFile {
    #[serde(default)]
    api_proxy: ApiProxyConfigPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayResponsesEndpoint {
    Responses,
    Compact,
}

impl RelayResponsesEndpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Responses => "/responses",
            Self::Compact => "/responses/compact",
        }
    }

    fn openai_url(self) -> &'static str {
        match self {
            Self::Responses => "https://chatgpt.com/backend-api/codex/responses",
            Self::Compact => "https://chatgpt.com/backend-api/codex/responses/compact",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RelayRegistryFile {
    #[serde(default)]
    active_provider_id: Option<String>,
    #[serde(default)]
    providers: Vec<RelayProviderRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RelayProviderRecord {
    id: String,
    name: String,
    base_url: String,
    api_key: Option<String>,
    #[serde(default)]
    env_key: Option<String>,
    model: String,
    wire_api: String,
    #[serde(default)]
    models_sample: Vec<String>,
    #[serde(default)]
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RelayConfigBackupFile {
    config_exists: bool,
    config_text: String,
}

pub fn relay_registry_path(paths: &CodexPaths) -> PathBuf {
    paths.codexmate_dir.join(RELAY_REGISTRY_FILE)
}

fn relay_config_backup_path(paths: &CodexPaths) -> PathBuf {
    paths.codexmate_dir.join(RELAY_CONFIG_BACKUP_FILE)
}

pub fn relay_model_catalog_path(paths: &CodexPaths) -> PathBuf {
    paths.codex_home.join("codex_router_catalog.json")
}

fn relay_model_catalog_filename() -> &'static str {
    "codex_router_catalog.json"
}

pub fn load_relay_state(paths: &CodexPaths) -> Result<RelayStatePayload, CoreError> {
    let registry = load_registry(paths)?;
    Ok(state_payload(paths, &registry))
}

pub fn upsert_relay_provider(
    paths: &CodexPaths,
    draft: RelayProviderDraftPayload,
) -> Result<RelayProviderPayload, CoreError> {
    let mut registry = load_registry(paths)?;
    let id = draft
        .id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_provider_id)
        .unwrap_or_else(|| sanitize_provider_id(&draft.name));
    if id.is_empty() {
        return Err(CoreError::InvalidData("Provider id is empty".to_string()));
    }

    let existing_api_key = registry
        .providers
        .iter()
        .find(|provider| provider.id == id)
        .and_then(|provider| provider.api_key.clone());
    let record = RelayProviderRecord {
        id: id.clone(),
        name: draft.name.trim().to_string(),
        base_url: normalize_base_url(&draft.base_url),
        api_key: draft
            .api_key
            .filter(|value| !value.trim().is_empty())
            .or(existing_api_key),
        env_key: None,
        model: draft.model.trim().to_string(),
        wire_api: normalize_wire_api(&draft.wire_api),
        models_sample: Vec::new(),
        last_error: None,
    };

    if record.name.is_empty() || record.base_url.is_empty() || record.model.is_empty() {
        return Err(CoreError::InvalidData(
            "Provider name, baseUrl, and model are required".to_string(),
        ));
    }
    if record.wire_api != "responses" {
        return Err(CoreError::InvalidData(
            "AiMaMi relay currently supports only the OpenAI Responses wire API.".to_string(),
        ));
    }

    if let Some(existing) = registry
        .providers
        .iter_mut()
        .find(|provider| provider.id == id)
    {
        *existing = record.clone();
    } else {
        registry.providers.push(record.clone());
    }

    save_registry(paths, &registry)?;
    Ok(provider_payload(
        &record,
        registry.active_provider_id.as_deref() == Some(record.id.as_str()),
    ))
}

pub fn activate_relay_provider(
    paths: &CodexPaths,
    provider_id: &str,
) -> Result<RelayStatePayload, CoreError> {
    let mut registry = load_registry(paths)?;
    let provider = registry
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .cloned()
        .ok_or_else(|| CoreError::NotFound(format!("Relay provider not found: {provider_id}")))?;
    if normalize_wire_api(&provider.wire_api) != "responses" {
        return Err(CoreError::InvalidData(
            "AiMaMi relay currently supports only the OpenAI Responses wire API.".to_string(),
        ));
    }
    write_codex_config(paths, &provider)?;
    if let Err(error) = crate::core::sessions::auto_sync_session_provider_buckets_to_active(paths) {
        eprintln!("[AiMaMi] session provider auto-sync failed after provider activation: {error}");
    }
    registry.active_provider_id = Some(provider.id);
    save_registry(paths, &registry)?;
    Ok(state_payload(paths, &registry))
}

pub fn delete_relay_provider(
    paths: &CodexPaths,
    provider_id: &str,
) -> Result<RelayStatePayload, CoreError> {
    let mut registry = load_registry(paths)?;
    registry.providers.retain(|provider| provider.id != provider_id);
    if registry.active_provider_id.as_deref() == Some(provider_id) {
        registry.active_provider_id = None;
        remove_codex_config(paths)?;
        if let Err(error) = crate::core::sessions::auto_sync_session_provider_buckets_to_active(paths) {
            eprintln!("[AiMaMi] session provider auto-sync failed after provider removal: {error}");
        }
    }
    save_registry(paths, &registry)?;
    Ok(state_payload(paths, &registry))
}

pub fn test_relay_draft(draft: RelayProviderDraftPayload) -> Result<RelayTestPayload, CoreError> {
    if normalize_wire_api(&draft.wire_api) != "responses" {
        return Err(CoreError::InvalidData(
            "AiMaMi relay currently supports only the OpenAI Responses wire API.".to_string(),
        ));
    }
    test_responses_draft(draft)
}

pub fn spawn_relay_proxy_server(paths: Arc<CodexPaths>) -> std::thread::JoinHandle<()> {
    spawn_relay_proxy_server_at(paths, "127.0.0.1:49735")
}

fn spawn_relay_proxy_server_at(
    paths: Arc<CodexPaths>,
    listen_addr: &'static str,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("[AiMaMi] failed to start relay runtime: {error}");
                return;
            }
        };

        runtime.block_on(async move {
            if let Err(error) = run_relay_proxy_server(paths, listen_addr).await {
                eprintln!("[AiMaMi] relay proxy stopped on {listen_addr}: {error}");
            }
        });
    })
}

async fn run_relay_proxy_server(
    paths: Arc<CodexPaths>,
    listen_addr: &str,
) -> Result<(), CoreError> {
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let paths = paths.clone();
        tokio::spawn(async move {
            let _ = serve_relay_connection(
                paths,
                stream,
                RelayResponsesEndpoint::Responses.openai_url().to_string(),
            )
            .await;
        });
    }
}

async fn serve_relay_connection(
    paths: Arc<CodexPaths>,
    mut stream: tokio::net::TcpStream,
    openai_responses_url: String,
) -> Result<(), CoreError> {
    let raw = read_http_request(&mut stream).await?;
    if let Err(error) =
        write_relay_response_for_request(&paths, &raw, &mut stream, &openai_responses_url).await
    {
        write_http_response(&mut stream, error_response(error)).await?;
    }
    Ok(())
}

fn test_responses_draft(draft: RelayProviderDraftPayload) -> Result<RelayTestPayload, CoreError> {
    let url = responses_url(&draft.base_url, RelayResponsesEndpoint::Responses)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut request = client.post(url).json(&serde_json::json!({
        "model": draft.model.trim(),
        "input": "ping",
    }));
    if let Some(api_key) = draft
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        request = request.bearer_auth(api_key);
    }

    let response = request.send()?;
    let status = response.status();
    let status_code = Some(status.as_u16());
    let text = response.text()?;
    if !status.is_success() {
        return Ok(RelayTestPayload {
            ok: false,
            status_code,
            models: Vec::new(),
            error_message: Some(text),
        });
    }

    Ok(RelayTestPayload {
        ok: true,
        status_code,
        models: vec![draft.model.trim().to_string()],
        error_message: None,
    })
}

async fn relay_http_response_for_request(
    paths: &CodexPaths,
    raw_request: &[u8],
) -> Result<RelayHttpResponse, CoreError> {
    let request = parse_http_request(raw_request)?;
    let path = normalize_relay_path(&request.path);
    match (request.method.as_str(), path) {
        ("GET", "/models") | ("GET", "/v1/models") => relay_models_response(paths),
        ("GET", "/health") | ("GET", "/v1/health") => Ok(relay_health_response(paths)),
        ("POST", "/responses") | ("POST", "/v1/responses") => relay_responses_response(
            paths,
            &request.headers,
            &request.body,
            RelayResponsesEndpoint::Responses,
            RelayResponsesEndpoint::Responses.openai_url(),
        )
        .await,
        ("POST", "/responses/compact") | ("POST", "/v1/responses/compact") => {
            relay_responses_response(
                paths,
                &request.headers,
                &request.body,
                RelayResponsesEndpoint::Compact,
                RelayResponsesEndpoint::Compact.openai_url(),
            )
            .await
        }
        _ => Ok(json_response(
            404,
            serde_json::json!({
                "error": {
                    "message": "AiMaMi relay route not found",
                    "type": "not_found"
                }
            }),
        )),
    }
}

async fn write_relay_response_for_request(
    paths: &CodexPaths,
    raw_request: &[u8],
    stream: &mut tokio::net::TcpStream,
    openai_responses_url: &str,
) -> Result<(), CoreError> {
    let request = parse_http_request(raw_request)?;
    let path = normalize_relay_path(&request.path);
    match (request.method.as_str(), path) {
        ("POST", "/responses") | ("POST", "/v1/responses") => {
            write_relay_responses_stream(
                paths,
                &request,
                stream,
                RelayResponsesEndpoint::Responses,
                openai_responses_url,
            )
            .await
        }
        ("POST", "/responses/compact") | ("POST", "/v1/responses/compact") => {
            let compact_url = openai_compact_url(openai_responses_url);
            write_relay_responses_stream(
                paths,
                &request,
                stream,
                RelayResponsesEndpoint::Compact,
                &compact_url,
            )
            .await
        }
        _ => {
            let response = relay_http_response_for_request(paths, raw_request).await?;
            write_http_response(stream, response).await?;
            Ok(())
        }
    }
}

fn relay_models_response(paths: &CodexPaths) -> Result<RelayHttpResponse, CoreError> {
    if !relay_catalog_owned_by_config(paths) {
        return Ok(json_response(
            200,
            serde_json::json!({
                "models": []
            }),
        ));
    }

    let catalog = load_router_catalog(paths)?;
    Ok(json_response(200, catalog))
}

fn relay_health_response(paths: &CodexPaths) -> RelayHttpResponse {
    json_response(
        200,
        serde_json::json!({
            "service": "aimami-relay",
            "ok": true,
            "codexHome": paths.codex_home.display().to_string()
        }),
    )
}

async fn relay_responses_response(
    paths: &CodexPaths,
    headers: &[(String, String)],
    body: &[u8],
    endpoint: RelayResponsesEndpoint,
    openai_responses_url: &str,
) -> Result<RelayHttpResponse, CoreError> {
    let payload: Value = serde_json::from_slice(body)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::InvalidData("Responses payload model is required".to_string()))?;

    let Some(provider) = active_provider_for_model(paths, model)? else {
        return forward_openai_responses_request(paths, openai_responses_url, headers, body).await;
    };

    if normalize_wire_api(&provider.wire_api) != "responses" {
        return Ok(json_response(
            400,
            serde_json::json!({
                "error": {
                    "message": "Active provider does not use the Responses wire API.",
                    "type": "invalid_provider"
                }
            }),
        ));
    }

    if provider_uses_chat_adapter(&provider) {
        return forward_provider_chat_responses_request(paths, &provider, body).await;
    }

    forward_provider_responses_request(paths, &provider, body, endpoint).await
}

async fn forward_provider_responses_request(
    paths: &CodexPaths,
    provider: &RelayProviderRecord,
    body: &[u8],
    endpoint: RelayResponsesEndpoint,
) -> Result<RelayHttpResponse, CoreError> {
    let url = responses_url(&provider.base_url, provider_responses_endpoint(endpoint))?;
    let client = relay_http_client(paths)?;
    let Some(api_key) = provider_api_key(provider) else {
        return Ok(json_response(
            401,
            serde_json::json!({
                "error": {
                    "message": "Active relay provider has no API key configured.",
                    "type": "missing_api_key"
                }
            }),
        ));
    };

    let response = send_rate_limited_provider_request(
        &client,
        provider,
        &url,
        &api_key,
        ProviderRequestBody::Bytes(body.to_vec()),
    )
    .await?;
    let status_code = response.response.status().as_u16();
    let content_type = response
        .response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let body = response.response.bytes().await?.to_vec();
    {
        let parsed = body_value(&body);
        token_usage::record_token_usage(
            paths,
            "",
            parsed.as_ref().and_then(|v| v.get("usage")),
            &provider.id,
        );
    }
    let body = normalize_provider_sse_body(provider, &content_type, body);
    Ok(RelayHttpResponse {
        status_code,
        content_type,
        body,
    })
}

async fn forward_provider_chat_responses_request(
    paths: &CodexPaths,
    provider: &RelayProviderRecord,
    body: &[u8],
) -> Result<RelayHttpResponse, CoreError> {
    let payload: Value = serde_json::from_slice(body)?;
    let chat_body = responses_to_chat_request_body(&payload)?;
    let url = chat_completions_url(&provider.base_url)?;
    let client = relay_http_client(paths)?;
    let Some(api_key) = provider_api_key(provider) else {
        return Ok(json_response(
            401,
            serde_json::json!({
                "error": {
                    "message": "Active relay provider has no API key configured.",
                    "type": "missing_api_key"
                }
            }),
        ));
    };

    let response = send_rate_limited_provider_request(
        &client,
        provider,
        &url,
        &api_key,
        ProviderRequestBody::Json(chat_body.clone()),
    )
    .await?;
    let status_code = response.response.status().as_u16();
    let content_type = response
        .response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let raw_body = response.response.bytes().await?.to_vec();
    let model_name = chat_body.get("model").and_then(Value::as_str).unwrap_or("model");
    let body = if is_event_stream(&content_type) {
        chat_sse_to_responses_sse(&raw_body, model_name)?
    } else {
        let parsed = body_value(&raw_body);
        token_usage::record_token_usage(paths, model_name, parsed.as_ref().and_then(|v| v.get("usage")), &provider.id);
        chat_json_to_responses_json(&raw_body, model_name)?
    };
    let content_type = if is_event_stream(&content_type) {
        "text/event-stream".to_string()
    } else {
        "application/json".to_string()
    };
    Ok(RelayHttpResponse {
        status_code,
        content_type,
        body,
    })
}

async fn forward_openai_responses_request(
    paths: &CodexPaths,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<RelayHttpResponse, CoreError> {
    let Some(authorization) = header_value(headers, "authorization") else {
        return Ok(json_response(
            401,
            serde_json::json!({
                "error": {
                    "message": "Codex did not send OpenAI authorization to AiMaMi relay.",
                    "type": "missing_openai_auth"
                }
            }),
        ));
    };
    let client = relay_http_client(paths)?;
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("authorization", authorization)
        .body(body.to_vec());
    for name in ["openai-organization", "openai-project", "openai-beta"] {
        if let Some(value) = header_value(headers, name) {
            request = request.header(name, value);
        }
    }
    let response = request.send().await?;
    let status_code = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let body = response.bytes().await?.to_vec();
    Ok(RelayHttpResponse {
        status_code,
        content_type,
        body,
    })
}

async fn write_relay_responses_stream(
    paths: &CodexPaths,
    request: &RelayHttpRequest,
    stream: &mut tokio::net::TcpStream,
    endpoint: RelayResponsesEndpoint,
    openai_responses_url: &str,
) -> Result<(), CoreError> {
    let payload: Value = serde_json::from_slice(&request.body)?;
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::InvalidData("Responses payload model is required".to_string()))?;

    if let Some(provider) = active_provider_for_model(paths, model)? {
        if normalize_wire_api(&provider.wire_api) != "responses" {
            write_http_response(
                stream,
                json_response(
                    400,
                    serde_json::json!({
                        "error": {
                            "message": "Active provider does not use the Responses wire API.",
                            "type": "invalid_provider"
                        }
                    }),
                ),
            )
            .await?;
            return Ok(());
        }
        if provider_uses_chat_adapter(&provider) {
            write_provider_chat_responses_stream(paths, &provider, &request.body, stream).await?;
            return Ok(());
        }
        let normalize_dashscope_sse = is_dashscope_provider(&provider);
        let mut upstream =
            match send_provider_responses_request(paths, &provider, &request.body, endpoint).await? {
            Some(response) => response,
            None => {
                write_http_response(
                    stream,
                    json_response(
                        401,
                        serde_json::json!({
                            "error": {
                                "message": "Active relay provider has no API key configured.",
                                "type": "missing_api_key"
                            }
                        }),
                    ),
                )
                .await?;
                return Ok(());
            }
            };
        write_upstream_response_stream(stream, &mut upstream.response, normalize_dashscope_sse).await
    } else {
        let mut upstream = match send_openai_responses_request(
            paths,
            openai_responses_url,
            &request.headers,
            &request.body,
        )
            .await?
        {
            Some(response) => response,
            None => {
                write_http_response(
                    stream,
                    json_response(
                        401,
                        serde_json::json!({
                            "error": {
                                "message": "Codex did not send OpenAI authorization to AiMaMi relay.",
                                "type": "missing_openai_auth"
                            }
                        }),
                    ),
                )
                .await?;
                return Ok(());
            }
        };
        write_upstream_response_stream(stream, &mut upstream, false).await
    }
}

async fn send_provider_responses_request(
    paths: &CodexPaths,
    provider: &RelayProviderRecord,
    body: &[u8],
    endpoint: RelayResponsesEndpoint,
) -> Result<Option<ProviderHttpResponse>, CoreError> {
    let url = responses_url(&provider.base_url, provider_responses_endpoint(endpoint))?;
    let client = relay_http_client(paths)?;
    let Some(api_key) = provider_api_key(provider) else {
        return Ok(None);
    };
    Ok(Some(
        send_rate_limited_provider_request(
            &client,
            provider,
            &url,
            &api_key,
            ProviderRequestBody::Bytes(body.to_vec()),
        )
        .await?,
    ))
}

async fn write_provider_chat_responses_stream(
    paths: &CodexPaths,
    provider: &RelayProviderRecord,
    body: &[u8],
    stream: &mut tokio::net::TcpStream,
) -> Result<(), CoreError> {
    use tokio::io::AsyncWriteExt;

    let payload: Value = serde_json::from_slice(body)?;
    let chat_body = responses_to_chat_request_body(&payload)?;
    let url = chat_completions_url(&provider.base_url)?;
    let client = relay_http_client(paths)?;
    let Some(api_key) = provider_api_key(provider) else {
        write_http_response(
            stream,
            json_response(
                401,
                serde_json::json!({
                    "error": {
                        "message": "Active relay provider has no API key configured.",
                        "type": "missing_api_key"
                    }
                }),
            ),
        )
        .await?;
        return Ok(());
    };

    let mut upstream = send_rate_limited_provider_request(
        &client,
        provider,
        &url,
        &api_key,
        ProviderRequestBody::Json(chat_body.clone()),
    )
    .await?;
    let status_code = upstream.response.status().as_u16();
    let content_type = upstream
        .response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    if !is_event_stream(&content_type) {
        let raw_body = upstream.response.bytes().await?.to_vec();
        let model_name = chat_body.get("model").and_then(Value::as_str).unwrap_or("model");
        let parsed = body_value(&raw_body);
        token_usage::record_token_usage(paths, model_name, parsed.as_ref().and_then(|v| v.get("usage")), &provider.id);
        let body = chat_json_to_responses_json(
            &raw_body,
            model_name,
        )?;
        write_http_response(
            stream,
            RelayHttpResponse {
                status_code,
                content_type: "application/json".to_string(),
                body,
            },
        )
        .await?;
        return Ok(());
    }

    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        status_code,
        status_text(status_code)
    );
    stream.write_all(headers.as_bytes()).await?;
    let mut adapter = ChatToResponsesSseAdapter::new(
        chat_body.get("model").and_then(Value::as_str).unwrap_or("model"),
    );
    while let Some(chunk) = upstream.response.chunk().await? {
        let output = adapter.push(chunk.as_ref())?;
        if !output.is_empty() {
            stream.write_all(&output).await?;
            stream.flush().await?;
        }
    }
    let output = adapter.finish()?;
    if !output.is_empty() {
        stream.write_all(&output).await?;
        stream.flush().await?;
    }
    stream.shutdown().await?;
    Ok(())
}

fn provider_responses_endpoint(_endpoint: RelayResponsesEndpoint) -> RelayResponsesEndpoint {
    RelayResponsesEndpoint::Responses
}

async fn send_openai_responses_request(
    paths: &CodexPaths,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Option<reqwest::Response>, CoreError> {
    let Some(authorization) = header_value(headers, "authorization") else {
        return Ok(None);
    };
    let client = relay_http_client(paths)?;
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("authorization", authorization)
        .body(body.to_vec());
    for name in ["openai-organization", "openai-project", "openai-beta"] {
        if let Some(value) = header_value(headers, name) {
            request = request.header(name, value);
        }
    }
    Ok(Some(request.send().await?))
}

async fn write_upstream_response_stream(
    stream: &mut tokio::net::TcpStream,
    upstream: &mut reqwest::Response,
    normalize_dashscope_sse: bool,
) -> Result<(), CoreError> {
    use tokio::io::AsyncWriteExt;

    let status_code = upstream.status().as_u16();
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let mut sse_normalizer = (normalize_dashscope_sse && is_event_stream(&content_type))
        .then(SseEventNormalizer::default);
    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
        status_code,
        status_text(status_code),
        content_type
    );
    stream.write_all(headers.as_bytes()).await?;
    while let Some(chunk) = upstream.chunk().await? {
        let chunk = if let Some(normalizer) = sse_normalizer.as_mut() {
            normalizer.push(chunk.as_ref())
        } else {
            chunk.to_vec()
        };
        if !chunk.is_empty() {
            stream.write_all(&chunk).await?;
            stream.flush().await?;
        }
    }
    if let Some(normalizer) = sse_normalizer.as_mut() {
        let chunk = normalizer.finish();
        if !chunk.is_empty() {
            stream.write_all(&chunk).await?;
            stream.flush().await?;
        }
    }
    stream.shutdown().await?;
    Ok(())
}

fn normalize_provider_sse_body(
    provider: &RelayProviderRecord,
    content_type: &str,
    body: Vec<u8>,
) -> Vec<u8> {
    if is_dashscope_provider(provider) && is_event_stream(content_type) {
        normalize_dashscope_responses_sse_bytes(&body)
    } else {
        body
    }
}

fn responses_to_chat_request_body(payload: &Value) -> Result<Value, CoreError> {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::InvalidData("Responses payload model is required".to_string()))?;
    let mut messages = Vec::new();
    if let Some(instructions) = payload
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        messages.push(serde_json::json!({
            "content": instructions,
            "role": "system"
        }));
    }
    append_responses_input_as_chat_messages(payload.get("input"), &mut messages);
    if messages.is_empty() {
        messages.push(serde_json::json!({
            "content": "",
            "role": "user"
        }));
    }

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": payload.get("stream").and_then(Value::as_bool).unwrap_or(false)
    });
    if let Some(object) = body.as_object_mut() {
        for key in ["temperature", "top_p", "presence_penalty", "frequency_penalty"] {
            if let Some(value) = payload.get(key) {
                object.insert(key.to_string(), value.clone());
            }
        }
        if let Some(tools) = responses_tools_to_chat_tools(payload.get("tools")) {
            object.insert("tools".to_string(), tools);
            if let Some(value) = payload.get("tool_choice") {
                object.insert("tool_choice".to_string(), value.clone());
            }
            if let Some(value) = payload.get("parallel_tool_calls") {
                object.insert("parallel_tool_calls".to_string(), value.clone());
            }
        }
        if let Some(value) = payload
            .get("max_output_tokens")
            .or_else(|| payload.get("max_completion_tokens"))
            .or_else(|| payload.get("max_tokens"))
        {
            object.insert("max_tokens".to_string(), value.clone());
        }
        apply_reasoning_effort_for_glm(object, payload.get("reasoning_effort"));
    }
    Ok(body)
}

fn apply_reasoning_effort_for_glm(body: &mut serde_json::Map<String, Value>, effort: Option<&Value>) {
    let Some(effort) = effort.and_then(Value::as_str) else {
        return;
    };
    match effort {
        "xhigh" | "high" => {
            body.insert("reasoning_effort".to_string(), Value::String("high".into()));
        }
        "medium" => {
            body.insert("reasoning_effort".to_string(), Value::String("low".into()));
        }
        "low" => {
            body.insert("enable_thinking".to_string(), Value::Bool(false));
        }
        _ => {}
    }
}

fn append_responses_input_as_chat_messages(input: Option<&Value>, messages: &mut Vec<Value>) {
    match input {
        Some(Value::String(text)) => {
            messages.push(serde_json::json!({
                "content": text,
                "role": "user"
            }));
        }
        Some(Value::Array(items)) => {
            let mut pending_tool_calls = Vec::new();
            for item in items {
                append_responses_input_item_as_chat_message(
                    item,
                    messages,
                    &mut pending_tool_calls,
                );
            }
            flush_pending_chat_tool_calls(messages, &mut pending_tool_calls);
        }
        Some(Value::Object(_)) => {
            let mut pending_tool_calls = Vec::new();
            if let Some(item) = input {
                append_responses_input_item_as_chat_message(
                    item,
                    messages,
                    &mut pending_tool_calls,
                );
            }
            flush_pending_chat_tool_calls(messages, &mut pending_tool_calls);
        }
        Some(value) if !value.is_null() => {
            messages.push(serde_json::json!({
                "content": value.to_string(),
                "role": "user"
            }));
        }
        _ => {}
    }
}

fn append_responses_input_item_as_chat_message(
    item: &Value,
    messages: &mut Vec<Value>,
    pending_tool_calls: &mut Vec<Value>,
) {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            pending_tool_calls.push(responses_function_call_to_chat_tool_call(item));
        }
        Some("custom_tool_call") => {
            pending_tool_calls.push(responses_custom_tool_call_to_chat_tool_call(item));
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            flush_pending_chat_tool_calls(messages, pending_tool_calls);
            messages.push(responses_tool_output_to_chat_message(item));
        }
        _ => {
            flush_pending_chat_tool_calls(messages, pending_tool_calls);
            if let Some(message) = responses_input_item_to_chat_message(item) {
                messages.push(message);
            }
        }
    }
}

fn flush_pending_chat_tool_calls(messages: &mut Vec<Value>, pending_tool_calls: &mut Vec<Value>) {
    if pending_tool_calls.is_empty() {
        return;
    }
    messages.push(serde_json::json!({
        "content": null,
        "role": "assistant",
        "tool_calls": std::mem::take(pending_tool_calls)
    }));
}

fn responses_input_item_to_chat_message(item: &Value) -> Option<Value> {
    let role = item
        .get("role")
        .and_then(Value::as_str)
        .map(normalize_chat_role)
        .unwrap_or("user");
    let content = responses_content_text(item.get("content"))
        .or_else(|| item.get("text").and_then(Value::as_str).map(ToString::to_string))
        .or_else(|| item.get("output").and_then(Value::as_str).map(ToString::to_string))?;
    Some(serde_json::json!({
        "content": content,
        "role": role
    }))
}

fn responses_function_call_to_chat_tool_call(item: &Value) -> Value {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
    serde_json::json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": tool_arguments_string(item.get("arguments"))
        }
    })
}

fn responses_custom_tool_call_to_chat_tool_call(item: &Value) -> Value {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
    let input = item.get("input").cloned().unwrap_or(Value::Null);
    serde_json::json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": tool_arguments_string(Some(&input))
        }
    })
}

fn responses_tool_output_to_chat_message(item: &Value) -> Value {
    let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
    let content = item
        .get("output")
        .or_else(|| item.get("result"))
        .map(tool_output_string)
        .unwrap_or_default();
    serde_json::json!({
        "content": content,
        "role": "tool",
        "tool_call_id": call_id
    })
}

fn responses_tools_to_chat_tools(tools: Option<&Value>) -> Option<Value> {
    let tools = tools.and_then(Value::as_array)?;
    let chat_tools = tools
        .iter()
        .filter_map(responses_tool_to_chat_tool)
        .collect::<Vec<_>>();
    (!chat_tools.is_empty()).then_some(Value::Array(chat_tools))
}

fn responses_tool_to_chat_tool(tool: &Value) -> Option<Value> {
    let tool_type = tool.get("type").and_then(Value::as_str).unwrap_or("function");
    if tool_type != "function" {
        return None;
    }
    if tool.get("function").is_some() {
        return Some(tool.clone());
    }
    let name = tool.get("name").and_then(Value::as_str)?;
    let mut function = serde_json::Map::new();
    function.insert("name".to_string(), Value::String(name.to_string()));
    for key in ["description", "parameters", "strict"] {
        if let Some(value) = tool.get(key) {
            function.insert(key.to_string(), value.clone());
        }
    }
    Some(serde_json::json!({
        "type": "function",
        "function": Value::Object(function)
    }))
}

fn tool_arguments_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) => serde_json::to_string(value).unwrap_or_default(),
        None => String::new(),
    }
}

fn tool_output_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        value => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn responses_content_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .or_else(|| part.get("input_text"))
                        .or_else(|| part.get("output_text"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn normalize_chat_role(role: &str) -> &'static str {
    match role {
        "assistant" => "assistant",
        "system" => "system",
        "developer" => "system",
        "tool" => "tool",
        _ => "user",
    }
}

fn chat_json_to_responses_json(body: &[u8], fallback_model: &str) -> Result<Vec<u8>, CoreError> {
    let value: Value = serde_json::from_slice(body)?;
    let id = chat_response_id(&value);
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_model);
    let created_at = value
        .get("created")
        .and_then(Value::as_u64)
        .unwrap_or_else(current_unix_timestamp);
    let message = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .unwrap_or(&Value::Null);
    let text = chat_message_text(message);
    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(assistant_message_item(&assistant_message_id(&id), &text));
    }
    output.extend(chat_message_tool_call_items(message));
    if output.is_empty() {
        output.push(assistant_message_item(&assistant_message_id(&id), ""));
    }
    let usage = value.get("usage").map(chat_usage_to_responses_usage);
    let response = completed_responses_json_with_output(&id, model, created_at, output, usage);
    Ok(serde_json::to_vec(&response)?)
}

fn chat_sse_to_responses_sse(body: &[u8], fallback_model: &str) -> Result<Vec<u8>, CoreError> {
    let mut adapter = ChatToResponsesSseAdapter::new(fallback_model);
    let mut output = adapter.push(body)?;
    output.extend(adapter.finish()?);
    Ok(output)
}

struct ChatToResponsesSseAdapter {
    pending: Vec<u8>,
    response_id: String,
    model: String,
    created_at: u64,
    text: String,
    text_output_index: Option<usize>,
    response_created: bool,
    text_started: bool,
    completed: bool,
    usage: Option<Value>,
    next_output_index: usize,
    tool_calls: BTreeMap<usize, StreamingToolCall>,
}

#[derive(Debug, Clone, Default)]
struct StreamingToolCall {
    call_id: String,
    name: String,
    arguments: String,
    item_id: String,
    output_index: Option<usize>,
    started: bool,
    done: bool,
}

impl ChatToResponsesSseAdapter {
    fn new(model: &str) -> Self {
        Self {
            pending: Vec::new(),
            response_id: "resp_aimami_chat".to_string(),
            model: model.to_string(),
            created_at: current_unix_timestamp(),
            text: String::new(),
            text_output_index: None,
            response_created: false,
            text_started: false,
            completed: false,
            usage: None,
            next_output_index: 0,
            tool_calls: BTreeMap::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>, CoreError> {
        self.pending.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some(end) = sse_block_end(&self.pending) {
            let rest = self.pending.split_off(end);
            let block = std::mem::replace(&mut self.pending, rest);
            output.extend(self.process_block(&block)?);
        }
        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<u8>, CoreError> {
        let pending = std::mem::take(&mut self.pending);
        let mut output = self.process_block(&pending)?;
        output.extend(self.finalize());
        Ok(output)
    }

    fn process_block(&mut self, block: &[u8]) -> Result<Vec<u8>, CoreError> {
        if block.is_empty() {
            return Ok(Vec::new());
        }
        let Ok(text) = std::str::from_utf8(block) else {
            return Ok(Vec::new());
        };
        let mut output = Vec::new();
        for data in sse_data_lines(text) {
            if data.trim() == "[DONE]" {
                output.extend(self.finalize());
                continue;
            }
            let value: Value = serde_json::from_str(&data)?;
            output.extend(self.process_chat_chunk(&value));
        }
        Ok(output)
    }

    fn process_chat_chunk(&mut self, chunk: &Value) -> Vec<u8> {
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            self.response_id = response_id_from_chat_id(id);
        }
        if let Some(model) = chunk.get("model").and_then(Value::as_str) {
            if !model.is_empty() {
                self.model = model.to_string();
            }
        }
        if let Some(created) = chunk.get("created").and_then(Value::as_u64) {
            self.created_at = created;
        }
        if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            self.usage = Some(chat_usage_to_responses_usage(usage));
        }

        let mut output = self.ensure_response_created();
        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first());
        if let Some(delta) = choice.and_then(|choice| choice.get("delta")) {
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    output.extend(self.push_text_delta(content));
                }
            }
            if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
                if !refusal.is_empty() {
                    output.extend(self.push_text_delta(refusal));
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                output.extend(self.push_tool_call_deltas(tool_calls));
            } else if let Some(function_call) =
                delta.get("function_call").filter(|value| !value.is_null())
            {
                output.extend(self.push_legacy_function_call_delta(function_call));
            }
        }
        if choice
            .and_then(|choice| choice.get("finish_reason"))
            .is_some_and(|value| !value.is_null())
        {
            output.extend(self.finalize());
        }
        output
    }

    fn ensure_response_created(&mut self) -> Vec<u8> {
        if self.response_created {
            return Vec::new();
        }
        self.response_created = true;
        sse_json_event(
            "response.created",
            serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": self.created_at,
                    "model": self.model,
                    "status": "in_progress",
                    "output": []
                }
            }),
        )
    }

    fn push_text_delta(&mut self, delta: &str) -> Vec<u8> {
        let mut output = self.ensure_text_started();
        self.text.push_str(delta);
        let output_index = self.text_output_index.unwrap_or(0);
        output.extend(sse_json_event(
            "response.output_text.delta",
            serde_json::json!({
                "type": "response.output_text.delta",
                "item_id": assistant_message_id(&self.response_id),
                "output_index": output_index,
                "content_index": 0,
                "delta": delta,
                "logprobs": []
            }),
        ));
        output
    }

    fn ensure_text_started(&mut self) -> Vec<u8> {
        if self.text_started {
            return Vec::new();
        }
        self.text_started = true;
        let output_index = self.next_output_index;
        self.next_output_index += 1;
        self.text_output_index = Some(output_index);
        let item_id = assistant_message_id(&self.response_id);
        let mut output = sse_json_event(
            "response.output_item.added",
            serde_json::json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "in_progress",
                    "content": []
                }
            }),
        );
        output.extend(sse_json_event(
            "response.content_part.added",
            serde_json::json!({
                "type": "response.content_part.added",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": "",
                    "annotations": []
                }
            }),
        ));
        output
    }

    fn push_tool_call_deltas(&mut self, tool_calls: &[Value]) -> Vec<u8> {
        let mut output = Vec::new();
        for (position, tool_call) in tool_calls.iter().enumerate() {
            let index = tool_call
                .get("index")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(position);
            output.extend(self.push_tool_call_delta(index, tool_call));
        }
        output
    }

    fn push_legacy_function_call_delta(&mut self, function_call: &Value) -> Vec<u8> {
        let tool_call = serde_json::json!({
            "index": 0,
            "id": function_call.get("id").and_then(Value::as_str).unwrap_or("call_0"),
            "type": "function",
            "function": function_call
        });
        self.push_tool_call_delta(0, &tool_call)
    }

    fn push_tool_call_delta(&mut self, index: usize, tool_call: &Value) -> Vec<u8> {
        let mut output = self.ensure_response_created();
        let function = tool_call.get("function").unwrap_or(&Value::Null);
        let call = self.tool_calls.entry(index).or_default();
        if let Some(call_id) = tool_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            call.call_id = call_id.to_string();
        }
        if call.call_id.is_empty() {
            call.call_id = format!("call_{index}");
        }
        if let Some(name) = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            call.name = name.to_string();
        }
        let argument_delta = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        let should_start = !call.started && !call.name.is_empty();
        if should_start {
            let output_index = self.next_output_index;
            self.next_output_index += 1;
            call.output_index = Some(output_index);
            call.item_id = function_call_item_id(&call.call_id);
            call.started = true;
            output.extend(sse_json_event(
                "response.output_item.added",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": function_call_item(
                        &call.item_id,
                        "in_progress",
                        &call.call_id,
                        &call.name,
                        &call.arguments
                    )
                }),
            ));
        }
        if !argument_delta.is_empty() {
            call.arguments.push_str(argument_delta);
            if call.started {
                output.extend(sse_json_event(
                    "response.function_call_arguments.delta",
                    serde_json::json!({
                        "type": "response.function_call_arguments.delta",
                        "item_id": call.item_id,
                        "output_index": call.output_index.unwrap_or(index),
                        "delta": argument_delta
                    }),
                ));
            }
        }
        output
    }

    fn finalize(&mut self) -> Vec<u8> {
        if self.completed {
            return Vec::new();
        }
        self.completed = true;
        let mut output = self.ensure_response_created();
        let has_tool_calls = !self.tool_calls.is_empty();
        if self.text_started || !has_tool_calls {
            output.extend(self.ensure_text_started());
            let item_id = assistant_message_id(&self.response_id);
            let output_index = self.text_output_index.unwrap_or(0);
            output.extend(sse_json_event(
                "response.output_text.done",
                serde_json::json!({
                    "type": "response.output_text.done",
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "text": self.text,
                    "logprobs": []
                }),
            ));
            output.extend(sse_json_event(
                "response.content_part.done",
                serde_json::json!({
                    "type": "response.content_part.done",
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": self.text,
                        "annotations": []
                    }
                }),
            ));
            output.extend(sse_json_event(
                "response.output_item.done",
                serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": assistant_message_item(&item_id, &self.text)
                }),
            ));
        }
        output.extend(self.finalize_tool_calls());
        let response_output = self.completed_output_items();
        output.extend(sse_json_event(
            "response.completed",
            serde_json::json!({
                "type": "response.completed",
                "response": completed_responses_json_with_output(
                    &self.response_id,
                    &self.model,
                    self.created_at,
                    response_output,
                    self.usage.clone()
                )
            }),
        ));
        output
    }

    fn finalize_tool_calls(&mut self) -> Vec<u8> {
        let mut output = Vec::new();
        for (index, call) in self.tool_calls.iter_mut() {
            if call.done {
                continue;
            }
            if !call.started {
                let output_index = self.next_output_index;
                self.next_output_index += 1;
                call.output_index = Some(output_index);
                if call.call_id.is_empty() {
                    call.call_id = format!("call_{index}");
                }
                call.item_id = function_call_item_id(&call.call_id);
                call.started = true;
                output.extend(sse_json_event(
                    "response.output_item.added",
                    serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": function_call_item(
                            &call.item_id,
                            "in_progress",
                            &call.call_id,
                            &call.name,
                            &call.arguments
                        )
                    }),
                ));
            }
            let output_index = call.output_index.unwrap_or(*index);
            output.extend(sse_json_event(
                "response.function_call_arguments.done",
                serde_json::json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": call.item_id,
                    "output_index": output_index,
                    "arguments": call.arguments
                }),
            ));
            output.extend(sse_json_event(
                "response.output_item.done",
                serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": function_call_item(
                        &call.item_id,
                        "completed",
                        &call.call_id,
                        &call.name,
                        &call.arguments
                    )
                }),
            ));
            call.done = true;
        }
        output
    }

    fn completed_output_items(&self) -> Vec<Value> {
        let mut items = Vec::new();
        if self.text_started || self.tool_calls.is_empty() {
            items.push((
                self.text_output_index.unwrap_or(0),
                assistant_message_item(&assistant_message_id(&self.response_id), &self.text),
            ));
        }
        for call in self.tool_calls.values() {
            items.push((
                call.output_index.unwrap_or(0),
                function_call_item(
                    &call.item_id,
                    "completed",
                    &call.call_id,
                    &call.name,
                    &call.arguments,
                ),
            ));
        }
        items.sort_by_key(|(output_index, _)| *output_index);
        items.into_iter().map(|(_, item)| item).collect()
    }
}

fn sse_data_lines(block: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("data:") {
            lines.push(value.trim_start().to_string());
        }
    }
    lines
}

fn sse_json_event(event: &str, data: Value) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(b"event: ");
    output.extend_from_slice(event.as_bytes());
    output.extend_from_slice(b"\n");
    output.extend_from_slice(b"data: ");
    output.extend_from_slice(
        serde_json::to_string(&data)
            .unwrap_or_else(|_| "{}".to_string())
            .as_bytes(),
    );
    output.extend_from_slice(b"\n\n");
    output
}

fn completed_responses_json(
    response_id: &str,
    model: &str,
    created_at: u64,
    text: &str,
    usage: Option<Value>,
) -> Value {
    completed_responses_json_with_output(
        response_id,
        model,
        created_at,
        vec![assistant_message_item(&assistant_message_id(response_id), text)],
        usage,
    )
}

fn completed_responses_json_with_output(
    response_id: &str,
    model: &str,
    created_at: u64,
    output: Vec<Value>,
    usage: Option<Value>,
) -> Value {
    let mut response = serde_json::json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "model": model,
        "status": "completed",
        "output": output
    });
    if let Some(usage) = usage {
        if let Some(object) = response.as_object_mut() {
            object.insert("usage".to_string(), usage);
        }
    }
    response
}

fn assistant_message_item(item_id: &str, text: &str) -> Value {
    serde_json::json!({
        "id": item_id,
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [{
            "type": "output_text",
            "text": text,
            "annotations": []
        }]
    })
}

fn chat_message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .or_else(|| part.get("input_text"))
                    .or_else(|| part.get("output_text"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => message
            .get("refusal")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

fn chat_message_tool_call_items(message: &Value) -> Vec<Value> {
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        return tool_calls
            .iter()
            .enumerate()
            .map(|(index, tool_call)| chat_tool_call_to_response_item(tool_call, index))
            .collect();
    }
    if let Some(function_call) = message.get("function_call").filter(|value| !value.is_null()) {
        return vec![chat_legacy_function_call_to_response_item(function_call)];
    }
    Vec::new()
}

fn chat_tool_call_to_response_item(tool_call: &Value, index: usize) -> Value {
    let call_id = tool_call
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("call_{index}"));
    let function = tool_call.get("function").unwrap_or(&Value::Null);
    let name = function.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = tool_arguments_string(function.get("arguments"));
    function_call_item(
        &function_call_item_id(&call_id),
        "completed",
        &call_id,
        name,
        &arguments,
    )
}

fn chat_legacy_function_call_to_response_item(function_call: &Value) -> Value {
    let call_id = function_call
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("call_0");
    let name = function_call
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = tool_arguments_string(function_call.get("arguments"));
    function_call_item(
        &function_call_item_id(call_id),
        "completed",
        call_id,
        name,
        &arguments,
    )
}

fn function_call_item_id(call_id: &str) -> String {
    format!("fc_{call_id}")
}

fn function_call_item(
    item_id: &str,
    status: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
) -> Value {
    serde_json::json!({
        "id": item_id,
        "type": "function_call",
        "status": status,
        "arguments": arguments,
        "call_id": call_id,
        "name": name
    })
}

fn chat_response_id(value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_str)
        .map(response_id_from_chat_id)
        .unwrap_or_else(|| "resp_aimami_chat".to_string())
}

fn response_id_from_chat_id(id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_string()
    } else {
        format!("resp_{id}")
    }
}

fn assistant_message_id(response_id: &str) -> String {
    format!("msg_{response_id}")
}

fn chat_usage_to_responses_usage(usage: &Value) -> Value {
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input_tokens + output_tokens);
    serde_json::json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": total_tokens,
        "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens_details": { "reasoning_tokens": 0 }
    })
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn is_event_stream(content_type: &str) -> bool {
    content_type
        .to_ascii_lowercase()
        .contains("text/event-stream")
}

#[derive(Default)]
struct SseEventNormalizer {
    pending: Vec<u8>,
}

impl SseEventNormalizer {
    fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some(end) = sse_block_end(&self.pending) {
            let rest = self.pending.split_off(end);
            let block = std::mem::replace(&mut self.pending, rest);
            output.extend(normalize_dashscope_responses_sse_block(&block));
        }
        output
    }

    fn finish(&mut self) -> Vec<u8> {
        let pending = std::mem::take(&mut self.pending);
        normalize_dashscope_responses_sse_block(&pending)
    }
}

fn normalize_dashscope_responses_sse_bytes(body: &[u8]) -> Vec<u8> {
    let mut normalizer = SseEventNormalizer::default();
    let mut output = normalizer.push(body);
    output.extend(normalizer.finish());
    output
}

fn sse_block_end(buffer: &[u8]) -> Option<usize> {
    [b"\r\n\r\n".as_slice(), b"\n\n".as_slice()]
        .into_iter()
        .filter_map(|delimiter| {
            buffer
                .windows(delimiter.len())
                .position(|window| window == delimiter)
                .map(|position| position + delimiter.len())
        })
        .min()
}

fn normalize_dashscope_responses_sse_block(block: &[u8]) -> Vec<u8> {
    if block.is_empty() {
        return Vec::new();
    }
    let normalized = replace_dashscope_event_names(block);
    let Ok(text) = std::str::from_utf8(&normalized) else {
        return normalized;
    };

    let mut event_name: Option<String> = None;
    let mut data_text: Option<String> = None;
    let mut passthrough_lines = Vec::new();

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            let value = value.trim().to_string();
            event_name = Some(value.clone());
            passthrough_lines.push(format!("event:{value}"));
        } else if let Some(value) = line.strip_prefix("data:") {
            data_text = Some(value.trim_start().to_string());
        } else {
            passthrough_lines.push(line.to_string());
        }
    }

    if event_name
        .as_deref()
        .is_some_and(|event| event.starts_with("response.reasoning_summary_text."))
    {
        return Vec::new();
    }

    if let Some(data) = data_text.as_deref() {
        if let Ok(mut value) = serde_json::from_str::<Value>(data) {
            if is_reasoning_output_item_event(event_name.as_deref(), &value) {
                return Vec::new();
            }
            normalize_response_data_for_desktop(&mut value);
            data_text = Some(match serde_json::to_string(&value) {
                Ok(value) => value,
                Err(_) => data.to_string(),
            });
        }
    }

    let mut output = Vec::new();
    for line in passthrough_lines {
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
    if let Some(data) = data_text {
        output.extend_from_slice(b"data: ");
        output.extend_from_slice(data.as_bytes());
        output.push(b'\n');
    }
    output.push(b'\n');
    output
}

fn replace_dashscope_event_names(body: &[u8]) -> Vec<u8> {
    dashscope_sse_event_replacements()
        .iter()
        .fold(body.to_vec(), |acc, (from, to)| {
            replace_bytes(&acc, from.as_bytes(), to.as_bytes())
        })
}

fn is_reasoning_output_item_event(event_name: Option<&str>, value: &Value) -> bool {
    matches!(
        event_name,
        Some("response.output_item.added" | "response.output_item.done")
    ) && value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        == Some("reasoning")
}

fn normalize_response_data_for_desktop(value: &mut Value) {
    decrement_output_indexes(value);
    if value.get("type").and_then(Value::as_str) == Some("response.completed") {
        if let Some(output) = value
            .get_mut("response")
            .and_then(|response| response.get_mut("output"))
            .and_then(Value::as_array_mut)
        {
            output.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
        }
    }
}

fn decrement_output_indexes(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(output_index) = object.get_mut("output_index") {
                if let Some(index) = output_index.as_i64() {
                    if index > 0 {
                        *output_index = Value::Number(serde_json::Number::from(index - 1));
                    }
                }
            }
            for child in object.values_mut() {
                decrement_output_indexes(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                decrement_output_indexes(item);
            }
        }
        _ => {}
    }
}

fn dashscope_sse_event_replacements() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "response-reasoning_summary_text-delta",
            "response.reasoning_summary_text.delta",
        ),
        (
            "response-reasoning_summary_text-done",
            "response.reasoning_summary_text.done",
        ),
        (
            "response-function_call_arguments-delta",
            "response.function_call_arguments.delta",
        ),
        (
            "response-function_call_arguments-done",
            "response.function_call_arguments.done",
        ),
        ("response-output_text-delta", "response.output_text.delta"),
        ("response-output_text-done", "response.output_text.done"),
        ("response-output_item-added", "response.output_item.added"),
        ("response-output_item-done", "response.output_item.done"),
        ("response-content_part-added", "response.content_part.added"),
        ("response-content_part-done", "response.content_part.done"),
        ("response-refusal-delta", "response.refusal.delta"),
        ("response-refusal-done", "response.refusal.done"),
        ("response-in_progress", "response.in_progress"),
        ("response-completed", "response.completed"),
        ("response-created", "response.created"),
        ("response-failed", "response.failed"),
    ]
}

fn replace_bytes(input: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() {
        return input.to_vec();
    }

    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index..].starts_with(from) {
            output.extend_from_slice(to);
            index += from.len();
        } else {
            output.push(input[index]);
            index += 1;
        }
    }
    output
}

fn load_registry(paths: &CodexPaths) -> Result<RelayRegistryFile, CoreError> {
    let path = relay_registry_path(paths);
    if !path.exists() {
        return Ok(RelayRegistryFile::default());
    }
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn load_router_catalog(paths: &CodexPaths) -> Result<Value, CoreError> {
    let path = relay_model_catalog_path(paths);
    if path.exists() {
        let raw = fs::read_to_string(path)?;
        return Ok(serde_json::from_str(&raw)?);
    }
    let registry = load_registry(paths)?;
    let provider = registry
        .active_provider_id
        .as_deref()
        .and_then(|id| registry.providers.iter().find(|provider| provider.id == id))
        .or_else(|| registry.providers.first())
        .cloned();
    Ok(provider
        .as_ref()
        .map(|provider| merged_model_catalog(paths, provider))
        .unwrap_or_else(|| serde_json::json!({ "models": fallback_official_models() })))
}

fn active_provider_for_model(
    paths: &CodexPaths,
    model: &str,
) -> Result<Option<RelayProviderRecord>, CoreError> {
    let registry = load_registry(paths)?;
    let Some(active_provider_id) = registry.active_provider_id.as_deref() else {
        return Ok(None);
    };
    let Some(provider) = registry
        .providers
        .iter()
        .find(|provider| provider.id == active_provider_id)
        .cloned()
    else {
        return Ok(None);
    };
    let is_provider_model = provider_model_slugs(&provider)
        .iter()
        .any(|slug| slug == model);
    if is_provider_model {
        Ok(Some(provider))
    } else {
        Ok(None)
    }
}

fn provider_api_key(provider: &RelayProviderRecord) -> Option<String> {
    provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn save_registry(paths: &CodexPaths, registry: &RelayRegistryFile) -> Result<(), CoreError> {
    fs::create_dir_all(&paths.codexmate_dir)?;
    let raw = serde_json::to_string_pretty(registry)?;
    fs::write(relay_registry_path(paths), raw)?;
    Ok(())
}

fn state_payload(paths: &CodexPaths, registry: &RelayRegistryFile) -> RelayStatePayload {
    RelayStatePayload {
        providers: registry
            .providers
            .iter()
            .map(|provider| {
                provider_payload(
                    provider,
                    registry.active_provider_id.as_deref() == Some(provider.id.as_str()),
                )
            })
            .collect(),
        active_provider_id: registry.active_provider_id.clone(),
        source_path: relay_registry_path(paths).display().to_string(),
        codex_config_path: paths.config_path.display().to_string(),
        diagnostics: relay_diagnostics(paths, registry),
    }
}

fn relay_diagnostics(
    paths: &CodexPaths,
    registry: &RelayRegistryFile,
) -> RelayDiagnosticsPayload {
    let config = fs::read_to_string(&paths.config_path).unwrap_or_default();
    let config_exists = paths.config_path.exists();
    let managed_block_present = has_relay_managed_blocks(&config);
    let catalog_ref = relay_model_catalog_config_ref(paths);
    let active_provider_configured = registry
        .active_provider_id
        .as_deref()
        .map(|id| {
            managed_block_present
                && config.contains(&format!(
                    "model_provider = {}",
                    quote_toml(RELAY_ROUTER_PROVIDER_ID)
                ))
                && config.contains(&format!("[model_providers.{RELAY_ROUTER_PROVIDER_ID}]"))
                && config.contains(&format!(
                    "model_catalog_json = {}",
                    quote_toml(&catalog_ref)
                ))
                && catalog_contains_provider_model(paths, registry, id)
        })
        .unwrap_or(false);
    let issue_message = if registry.active_provider_id.is_some() && !active_provider_configured {
        Some("Active provider is not configured in Codex config".to_string())
    } else if managed_block_present && registry.active_provider_id.is_none() {
        Some("Codex config has a relay block but no active provider is selected".to_string())
    } else {
        None
    };

    RelayDiagnosticsPayload {
        registry_exists: relay_registry_path(paths).exists(),
        config_exists,
        managed_block_present,
        active_provider_configured,
        relay_server_reachable: relay_server_reachable(paths),
        issue_message,
    }
}

fn relay_server_reachable(paths: &CodexPaths) -> bool {
    let Ok(addr) = "127.0.0.1:49735".parse::<SocketAddr>() else {
        return false;
    };
    relay_server_reachable_at(paths, addr, Duration::from_millis(100))
}

fn relay_server_reachable_at(paths: &CodexPaths, addr: SocketAddr, timeout: Duration) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    if stream.write_all(request).is_err() {
        return false;
    }
    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return false;
    }
    let response = String::from_utf8_lossy(&response);
    if !response.starts_with("HTTP/1.1 200") {
        return false;
    }
    let Some((_, body)) = response.split_once("\r\n\r\n") else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let expected_codex_home = paths.codex_home.display().to_string();
    value.get("service").and_then(Value::as_str) == Some("aimami-relay")
        && value.get("ok").and_then(Value::as_bool) == Some(true)
        && value.get("codexHome").and_then(Value::as_str) == Some(expected_codex_home.as_str())
}

fn provider_payload(provider: &RelayProviderRecord, active: bool) -> RelayProviderPayload {
    RelayProviderPayload {
        id: provider.id.clone(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        api_key_stored: provider.api_key.as_ref().is_some_and(|value| !value.is_empty()),
        model: provider.model.clone(),
        wire_api: provider.wire_api.clone(),
        active,
        models_sample: provider.models_sample.clone(),
        last_error: provider.last_error.clone(),
    }
}

fn write_codex_config(paths: &CodexPaths, provider: &RelayProviderRecord) -> Result<(), CoreError> {
    if let Some(parent) = paths.config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let config_exists = paths.config_path.exists();
    let current = fs::read_to_string(&paths.config_path).unwrap_or_default();
    save_config_backup_if_needed(paths, config_exists, &current)?;
    write_model_catalog(paths, provider)?;
    let next = append_relay_managed_block(
        &strip_relay_managed_block(&current),
        provider,
        Some(relay_model_catalog_path(paths)),
    );
    fs::write(&paths.config_path, next)?;
    Ok(())
}

fn remove_codex_config(paths: &CodexPaths) -> Result<(), CoreError> {
    if restore_config_backup(paths)? {
        return Ok(());
    }
    if !paths.config_path.exists() {
        return Ok(());
    }
    let current = fs::read_to_string(&paths.config_path)?;
    let next = strip_relay_managed_block(&current);
    if next != current {
        fs::write(&paths.config_path, next)?;
    }
    Ok(())
}

fn save_config_backup_if_needed(
    paths: &CodexPaths,
    config_exists: bool,
    current: &str,
) -> Result<(), CoreError> {
    if has_relay_managed_blocks(current) {
        return Ok(());
    }
    fs::create_dir_all(&paths.codexmate_dir)?;
    let backup = RelayConfigBackupFile {
        config_exists,
        config_text: current.to_string(),
    };
    fs::write(
        relay_config_backup_path(paths),
        serde_json::to_string_pretty(&backup)?,
    )?;
    Ok(())
}

fn restore_config_backup(paths: &CodexPaths) -> Result<bool, CoreError> {
    let backup_path = relay_config_backup_path(paths);
    if !backup_path.exists() {
        return Ok(false);
    }
    let backup: RelayConfigBackupFile =
        serde_json::from_str(&fs::read_to_string(&backup_path)?)?;
    if backup.config_exists {
        if let Some(parent) = paths.config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&paths.config_path, backup.config_text)?;
    } else if paths.config_path.exists() {
        fs::remove_file(&paths.config_path)?;
    }
    fs::remove_file(backup_path)?;
    Ok(true)
}

fn append_relay_managed_block(
    content: &str,
    _provider: &RelayProviderRecord,
    catalog_path: Option<PathBuf>,
) -> String {
    let mut top = Vec::new();
    top.push(RELAY_TOP_MANAGED_BLOCK_BEGIN.to_string());
    top.push(format!(
        "model_provider = {}",
        quote_toml(RELAY_ROUTER_PROVIDER_ID)
    ));
    if let Some(catalog_path) = catalog_path {
        top.push(format!(
            "model_catalog_json = {}",
            quote_toml(&catalog_path_for_config(&catalog_path))
        ));
    }
    top.push(RELAY_TOP_MANAGED_BLOCK_END.to_string());

    let mut provider_block = Vec::new();
    provider_block.push(RELAY_PROVIDER_MANAGED_BLOCK_BEGIN.to_string());
    provider_block.push(format!("[model_providers.{RELAY_ROUTER_PROVIDER_ID}]"));
    provider_block.push("name = \"AiMaMi Relay\"".to_string());
    provider_block.push(format!("base_url = {}", quote_toml(RELAY_ROUTER_BASE_URL)));
    provider_block.push("requires_openai_auth = true".to_string());
    provider_block.push("supports_websockets = false".to_string());
    provider_block.push("wire_api = \"responses\"".to_string());
    provider_block.push(RELAY_PROVIDER_MANAGED_BLOCK_END.to_string());

    let cleaned_content = remove_root_router_keys(content);
    let content = cleaned_content.trim_end_matches(['\r', '\n']);
    let with_top = insert_before_first_table(content, &top.join("\n"));
    let mut next = with_top.trim_end_matches(['\r', '\n']).to_string();
    if !next.is_empty() {
        next.push_str("\n\n");
    }
    next.push_str(&provider_block.join("\n"));
    next.push('\n');
    next
}

fn write_model_catalog(paths: &CodexPaths, provider: &RelayProviderRecord) -> Result<(), CoreError> {
    let path = relay_model_catalog_path(paths);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let catalog = merged_model_catalog(paths, provider);
    fs::write(path, serde_json::to_string_pretty(&catalog)?)?;
    Ok(())
}

fn merged_model_catalog(paths: &CodexPaths, provider: &RelayProviderRecord) -> Value {
    let mut models = load_codex_cached_models(paths)
        .or_else(load_codex_bundled_models)
        .unwrap_or_else(fallback_official_models);
    let template = find_catalog_template(&models);
    let custom_models = provider_catalog_models(provider, template.as_ref());
    models.retain(|model| {
        let slug = model.get("slug").and_then(Value::as_str);
        !custom_models.iter().any(|custom| custom.get("slug").and_then(Value::as_str) == slug)
    });
    models.extend(custom_models);
    serde_json::json!({ "models": models })
}

fn load_codex_cached_models(paths: &CodexPaths) -> Option<Vec<Value>> {
    let path = paths.codex_home.join("models_cache.json");
    let raw = fs::read_to_string(path).ok()?;
    parse_codex_bundled_models_output(&raw)
}

fn load_codex_bundled_models() -> Option<Vec<Value>> {
    let output = Command::new("codex")
        .args(["debug", "models", "--bundled"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_codex_bundled_models_output(&stdout)
}

fn parse_codex_bundled_models_output(output: &str) -> Option<Vec<Value>> {
    let trimmed = output.trim();
    let json_text = if trimmed.starts_with('{') {
        trimmed
    } else {
        let start = trimmed.find('{')?;
        let end = trimmed.rfind('}')?;
        trimmed.get(start..=end)?
    };
    let value: Value = serde_json::from_str(json_text).ok()?;
    value.get("models")?.as_array().cloned()
}

fn fallback_official_models() -> Vec<Value> {
    ["gpt-5.5", "gpt-5.4-mini", "gpt-5.3-codex-spark"]
        .into_iter()
        .enumerate()
        .map(|(index, slug)| catalog_model(slug, slug, "OpenAI Codex model", index as i64))
        .collect()
}

fn find_catalog_template(models: &[Value]) -> Option<Value> {
    models
        .iter()
        .find(|model| model.get("slug").and_then(Value::as_str) == Some("gpt-5.5"))
        .or_else(|| models.first())
        .cloned()
}

fn provider_model_specs(provider: &RelayProviderRecord) -> Vec<(String, String, String)> {
    if is_dashscope_provider(provider) {
        return [
            ("qwen3.7-max", "Qwen3.7 Max", "DashScope model"),
            ("qwen3.7-plus", "Qwen3.7 Plus", "DashScope model"),
            ("glm-5.2", "GLM-5.2", "DashScope model"),
        ]
        .into_iter()
        .map(|(slug, name, description)| {
            (slug.to_string(), name.to_string(), description.to_string())
        })
        .collect();
    }

    vec![(
        provider.model.clone(),
        provider.model.clone(),
        format!("{} model", provider.name),
    )]
}

fn is_dashscope_provider(provider: &RelayProviderRecord) -> bool {
    let base_url = provider.base_url.trim().to_ascii_lowercase();
    provider.id == "dashscope" || base_url.contains("dashscope.aliyuncs.com")
}

fn provider_uses_chat_adapter(provider: &RelayProviderRecord) -> bool {
    is_dashscope_provider(provider)
}

fn provider_model_slugs(provider: &RelayProviderRecord) -> Vec<String> {
    provider_model_specs(provider)
        .into_iter()
        .map(|(slug, _, _)| slug)
        .collect()
}

fn provider_catalog_models(
    provider: &RelayProviderRecord,
    template: Option<&Value>,
) -> Vec<Value> {
    provider_model_specs(provider)
        .into_iter()
        .enumerate()
        .map(|(index, (slug, display_name, description))| {
            catalog_model_from_template(
                template,
                &slug,
                &display_name,
                &description,
                100 + index as i64,
            )
        })
        .collect()
}

fn catalog_model_from_template(
    template: Option<&Value>,
    slug: &str,
    display_name: &str,
    description: &str,
    priority: i64,
) -> Value {
    let Some(template) = template else {
        return catalog_model(slug, display_name, description, priority);
    };
    let mut entry = template.clone();
    let Some(object) = entry.as_object_mut() else {
        return catalog_model(slug, display_name, description, priority);
    };

    object.insert("slug".to_string(), serde_json::json!(slug));
    object.insert("display_name".to_string(), serde_json::json!(display_name));
    object.insert("description".to_string(), serde_json::json!(description));
    object.insert("priority".to_string(), serde_json::json!(priority));
    object.insert("additional_speed_tiers".to_string(), serde_json::json!([]));
    object.insert("service_tiers".to_string(), serde_json::json!([]));
    object.insert("availability_nux".to_string(), Value::Null);
    object.insert("upgrade".to_string(), Value::Null);
    object.insert(
        "supports_reasoning_summaries".to_string(),
        serde_json::json!(false),
    );
    object.insert(
        "default_reasoning_summary".to_string(),
        serde_json::json!("none"),
    );
    object.insert("support_verbosity".to_string(), serde_json::json!(false));
    object.remove("default_verbosity");
    apply_model_context_window(object, slug);
    entry
}

fn apply_model_context_window(object: &mut serde_json::Map<String, Value>, slug: &str) {
    let (ctx, max_ctx) = match slug {
        "glm-5.2" => (1_048_576, 1_048_576),
        _ => return,
    };
    object.insert("context_window".to_string(), serde_json::json!(ctx));
    object.insert("max_context_window".to_string(), serde_json::json!(max_ctx));
}

fn catalog_model(slug: &str, display_name: &str, description: &str, priority: i64) -> Value {
    serde_json::json!({
        "slug": slug,
        "display_name": display_name,
        "description": description,
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            { "effort": "low", "description": "Fast responses with lighter reasoning" },
            { "effort": "medium", "description": "Balances speed and reasoning depth" },
            { "effort": "high", "description": "Greater reasoning depth" },
            { "effort": "xhigh", "description": "Extra high reasoning depth for complex problems" }
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": priority,
        "base_instructions": "You are Codex, a coding agent. You and the user share one workspace, and your job is to collaborate with them until their goal is genuinely handled.",
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "truncation_policy": { "mode": "tokens", "limit": 10000 },
        "supports_parallel_tool_calls": true,
        "experimental_supported_tools": []
    })
}

fn strip_relay_managed_block(content: &str) -> String {
    let mut output = Vec::new();
    let mut skipping = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == RELAY_MANAGED_BLOCK_BEGIN
            || trimmed == RELAY_TOP_MANAGED_BLOCK_BEGIN
            || trimmed == RELAY_PROVIDER_MANAGED_BLOCK_BEGIN
        {
            skipping = true;
            continue;
        }
        if skipping {
            if trimmed == RELAY_MANAGED_BLOCK_END
                || trimmed == RELAY_TOP_MANAGED_BLOCK_END
                || trimmed == RELAY_PROVIDER_MANAGED_BLOCK_END
            {
                skipping = false;
            }
            continue;
        }
        output.push(line.to_string());
    }
    let mut next = output.join("\n");
    while next.ends_with("\n\n\n") {
        next.pop();
    }
    if !next.is_empty() {
        next.push('\n');
    }
    next
}

fn has_relay_managed_blocks(content: &str) -> bool {
    let has_legacy =
        content.contains(RELAY_MANAGED_BLOCK_BEGIN) && content.contains(RELAY_MANAGED_BLOCK_END);
    let has_split = content.contains(RELAY_TOP_MANAGED_BLOCK_BEGIN)
        && content.contains(RELAY_TOP_MANAGED_BLOCK_END);
    has_legacy || has_split
}

fn insert_before_first_table(content: &str, block: &str) -> String {
    let lines = content.lines().collect::<Vec<_>>();
    let insert_at = lines
        .iter()
        .position(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with('[')
        })
        .unwrap_or(lines.len());

    let mut output = Vec::new();
    output.extend(lines[..insert_at].iter().map(|line| (*line).to_string()));
    if !output.is_empty() && output.last().is_some_and(|line| !line.trim().is_empty()) {
        output.push(String::new());
    }
    output.extend(block.lines().map(ToString::to_string));
    if insert_at < lines.len() {
        output.push(String::new());
        output.extend(lines[insert_at..].iter().map(|line| (*line).to_string()));
    }
    output.join("\n")
}

fn remove_root_router_keys(content: &str) -> String {
    let mut output = Vec::new();
    let mut in_root = true;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_root = false;
        }
        if in_root
            && (trimmed.starts_with("model_provider =")
                || trimmed.starts_with("model_catalog_json ="))
        {
            continue;
        }
        output.push(line.to_string());
    }
    output.join("\n")
}

fn catalog_contains_provider_model(
    paths: &CodexPaths,
    registry: &RelayRegistryFile,
    provider_id: &str,
) -> bool {
    let Some(provider) = registry.providers.iter().find(|provider| provider.id == provider_id)
    else {
        return false;
    };
    let Ok(raw) = fs::read_to_string(relay_model_catalog_path(paths)) else {
        return false;
    };
    let Ok(catalog) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    let slugs = provider_model_slugs(provider);
    catalog
        .get("models")
        .and_then(Value::as_array)
        .is_some_and(|models| {
            models.iter().any(|model| {
                model
                    .get("slug")
                    .and_then(Value::as_str)
                    .is_some_and(|slug| slugs.iter().any(|candidate| candidate == slug))
            })
        })
}

fn relay_catalog_owned_by_config(paths: &CodexPaths) -> bool {
    let Ok(config) = fs::read_to_string(&paths.config_path) else {
        return false;
    };
    let Ok(parsed) = config.parse::<toml::Value>() else {
        return false;
    };
    let expected_catalog = relay_model_catalog_path(paths);
    let expected_catalog_ref = relay_model_catalog_config_ref(paths);
    let provider = parsed
        .get("model_providers")
        .and_then(|providers| providers.get(RELAY_ROUTER_PROVIDER_ID));

    parsed.get("model_provider").and_then(|value| value.as_str())
        == Some(RELAY_ROUTER_PROVIDER_ID)
        && parsed
            .get("model_catalog_json")
            .and_then(|value| value.as_str())
            .is_some_and(|value| {
                value == expected_catalog_ref
                    || value == expected_catalog.display().to_string()
                    || std::path::Path::new(value)
                        .file_name()
                        .and_then(|name| name.to_str())
                        == Some(relay_model_catalog_filename())
            })
        && provider
            .and_then(|provider| provider.get("base_url"))
            .and_then(|value| value.as_str())
            == Some(RELAY_ROUTER_BASE_URL)
        && provider
            .and_then(|provider| provider.get("wire_api"))
            .and_then(|value| value.as_str())
            == Some("responses")
}

fn relay_model_catalog_config_ref(_paths: &CodexPaths) -> String {
    relay_model_catalog_filename().to_string()
}

fn catalog_path_for_config(catalog_path: &std::path::Path) -> String {
    catalog_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| *name == relay_model_catalog_filename())
        .unwrap_or_else(|| relay_model_catalog_filename())
        .to_string()
}

fn responses_url(
    base_url: &str,
    endpoint: RelayResponsesEndpoint,
) -> Result<String, CoreError> {
    let normalized = normalize_base_url(base_url);
    if normalized.is_empty() {
        return Err(CoreError::InvalidData("Provider baseUrl is empty".to_string()));
    }
    if normalized.ends_with("/responses/compact") {
        if endpoint == RelayResponsesEndpoint::Compact {
            Ok(normalized)
        } else {
            Ok(normalized
                .trim_end_matches("/compact")
                .to_string())
        }
    } else if normalized.ends_with("/responses") {
        if endpoint == RelayResponsesEndpoint::Compact {
            Ok(format!("{normalized}/compact"))
        } else {
            Ok(normalized)
        }
    } else if normalized.ends_with("/v1") {
        Ok(format!("{normalized}{}", endpoint.path()))
    } else {
        Ok(format!("{normalized}/v1{}", endpoint.path()))
    }
}

fn chat_completions_url(base_url: &str) -> Result<String, CoreError> {
    let normalized = normalize_base_url(base_url);
    if normalized.is_empty() {
        return Err(CoreError::InvalidData("Provider baseUrl is empty".to_string()));
    }
    if normalized.ends_with("/chat/completions") {
        Ok(normalized)
    } else if normalized.ends_with("/v1") {
        Ok(format!("{normalized}/chat/completions"))
    } else {
        Ok(format!("{normalized}/v1/chat/completions"))
    }
}

fn openai_compact_url(openai_responses_url: &str) -> String {
    let normalized = openai_responses_url.trim_end_matches('/');
    if normalized.ends_with("/responses/compact") {
        normalized.to_string()
    } else if normalized.ends_with("/responses") {
        format!("{normalized}/compact")
    } else {
        format!("{normalized}/responses/compact")
    }
}

fn parse_http_request(raw_request: &[u8]) -> Result<RelayHttpRequest, CoreError> {
    let header_end = find_header_end(raw_request)
        .ok_or_else(|| CoreError::InvalidData("HTTP request headers are incomplete".to_string()))?;
    let header_text = String::from_utf8_lossy(&raw_request[..header_end]);
    let mut lines = header_text.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| CoreError::InvalidData("HTTP request line is missing".to_string()))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| CoreError::InvalidData("HTTP method is missing".to_string()))?
        .to_ascii_uppercase();
    let path = parts
        .next()
        .ok_or_else(|| CoreError::InvalidData("HTTP path is missing".to_string()))?
        .to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    let body = raw_request[header_end..].to_vec();
    Ok(RelayHttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn normalize_relay_path(path: &str) -> &str {
    let path = path.split('?').next().unwrap_or(path);
    match path {
        "/v1/v1/models" | "/codex/v1/models" => "/v1/models",
        "/v1/v1/responses" | "/codex/v1/responses" => "/v1/responses",
        "/v1/v1/responses/compact" | "/codex/v1/responses/compact" => "/v1/responses/compact",
        _ => path,
    }
}

async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<Vec<u8>, std::io::Error> {
    use tokio::io::AsyncReadExt;

    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = find_header_end(&buffer) {
            let content_length = content_length(&buffer[..header_end]);
            if buffer.len() >= header_end + content_length {
                break;
            }
        }
        if buffer.len() > 16 * 1024 * 1024 {
            break;
        }
    }
    Ok(buffer)
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    response: RelayHttpResponse,
) -> Result<(), std::io::Error> {
    use tokio::io::AsyncWriteExt;

    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status_code,
        status_text(response.status_code),
        response.content_type,
        response.body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.shutdown().await
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| position + 2)
        })
}

fn content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn status_text(status_code: u16) -> &'static str {
    match status_code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        _ => "OK",
    }
}

fn json_response(status_code: u16, body: Value) -> RelayHttpResponse {
    RelayHttpResponse {
        status_code,
        content_type: "application/json".to_string(),
        body: serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

fn error_response(error: CoreError) -> RelayHttpResponse {
    json_response(
        502,
        serde_json::json!({
            "error": {
                "message": error.to_string(),
                "type": "relay_error"
            }
        }),
    )
}

async fn send_rate_limited_provider_request(
    client: &reqwest::Client,
    provider: &RelayProviderRecord,
    url: &str,
    api_key: &str,
    body: ProviderRequestBody,
) -> Result<ProviderHttpResponse, CoreError> {
    let mut retry_after: Option<Duration> = None;
    for attempt in 0..=PROVIDER_RETRY_DELAYS.len() {
        if attempt > 0 {
            let delay = retry_after.unwrap_or(PROVIDER_RETRY_DELAYS[attempt - 1]);
            tokio::time::sleep(delay).await;
        }
        wait_for_provider_rate_slot(provider).await;
        let permit = acquire_provider_request_permit(provider).await?;
        let response = build_provider_request(client, url, api_key, &body)
            .send()
            .await?;
        if response.status().as_u16() != 429 || attempt == PROVIDER_RETRY_DELAYS.len() {
            return Ok(ProviderHttpResponse {
                response,
                _permit: permit,
            });
        }
        retry_after = parse_retry_after(response.headers());
    }
    unreachable!("provider retry loop always returns")
}

async fn acquire_provider_request_permit(
    provider: &RelayProviderRecord,
) -> Result<tokio::sync::OwnedSemaphorePermit, CoreError> {
    let key = provider_limit_key(provider);
    let semaphore = {
        let limits = PROVIDER_CONCURRENCY_LIMITS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut limits = limits.lock().unwrap();
        limits
            .entry(key)
            .or_insert_with(|| {
                Arc::new(tokio::sync::Semaphore::new(
                    PROVIDER_MAX_CONCURRENT_REQUESTS,
                ))
            })
            .clone()
    };
    semaphore
        .acquire_owned()
        .await
        .map_err(|_| CoreError::OperationFailed("Provider request limiter closed".to_string()))
}

fn build_provider_request<'a>(
    client: &'a reqwest::Client,
    url: &'a str,
    api_key: &'a str,
    body: &'a ProviderRequestBody,
) -> reqwest::RequestBuilder {
    let request = client
        .post(url)
        .header("content-type", "application/json")
        .bearer_auth(api_key);
    match body {
        ProviderRequestBody::Bytes(body) => request.body(body.clone()),
        ProviderRequestBody::Json(body) => request.json(body),
    }
}

async fn wait_for_provider_rate_slot(provider: &RelayProviderRecord) {
    let key = provider_limit_key(provider);
    let sleep_for = {
        let now = Instant::now();
        let slots = PROVIDER_RATE_SLOTS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut slots = slots.lock().unwrap();
        let next_available = slots.entry(key).or_insert(now);
        if *next_available <= now {
            *next_available = now + PROVIDER_REQUEST_INTERVAL;
            None
        } else {
            let delay = *next_available - now;
            *next_available += PROVIDER_REQUEST_INTERVAL;
            Some(delay)
        }
    };
    if let Some(delay) = sleep_for {
        tokio::time::sleep(delay).await;
    }
}

fn provider_limit_key(provider: &RelayProviderRecord) -> String {
    format!("{}|{}", provider.id, provider.base_url)
}

fn body_value(body: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(body).ok()
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers
        .get("retry-after")
        .or_else(|| headers.get("Retry-After"))?;
    let value = value.to_str().ok()?;
    // Retry-After can be seconds (e.g. "120") or HTTP-date; we only handle seconds.
    let seconds: u64 = value.trim().parse().ok()?;
    if seconds == 0 {
        None
    } else {
        Some(Duration::from_secs(seconds))
    }
}

struct ProviderHttpResponse {
    response: reqwest::Response,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

#[derive(Clone)]
enum ProviderRequestBody {
    Bytes(Vec<u8>),
    Json(Value),
}

fn relay_http_client(paths: &CodexPaths) -> Result<reqwest::Client, CoreError> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(120));
    let proxy_config = load_relay_api_proxy_config(paths);
    let proxy_config = crate::core::api_client::sanitize_proxy_config(&proxy_config)?;
    if let Some(url) = proxy_config.url.as_deref() {
        builder = builder.proxy(reqwest::Proxy::all(url)?);
    }
    Ok(builder.build()?)
}

fn load_relay_api_proxy_config(paths: &CodexPaths) -> ApiProxyConfigPayload {
    let Ok(raw) = fs::read_to_string(&paths.settings_path) else {
        return ApiProxyConfigPayload::default();
    };
    serde_json::from_str::<RelaySettingsFile>(&raw)
        .map(|settings| settings.api_proxy)
        .unwrap_or_default()
}

fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn normalize_wire_api(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "responses".to_string()
    } else {
        value.to_string()
    }
}

fn sanitize_provider_id(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn quote_toml(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;

    fn paths(label: &str) -> (CodexPaths, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "aimami-relay-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let paths = CodexPaths::from_home(root.clone());
        fs::create_dir_all(&paths.codexmate_dir).unwrap();
        (paths, root)
    }

    #[test]
    fn upsert_provider_persists_registry_without_echoing_api_key() {
        let (paths, root) = paths("registry");

        let provider = upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("secret-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();

        assert_eq!(provider.id, "dashscope");
        assert!(provider.api_key_stored);
        assert!(!serde_json::to_string(&provider).unwrap().contains("secret-key"));

        let state = load_relay_state(&paths).unwrap();
        assert_eq!(state.providers.len(), 1);
        assert!(fs::read_to_string(relay_registry_path(&paths))
            .unwrap()
            .contains("secret-key"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn upsert_provider_rejects_non_responses_wire_api() {
        let (paths, root) = paths("reject-chat-wire-api");

        let error = upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("openrouter".into()),
                name: "OpenRouter".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key: Some("sk-or".into()),
                model: "anthropic/claude-3.5-sonnet".into(),
                wire_api: "openai-chat".into(),
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("Responses wire API"));
        assert!(!relay_registry_path(&paths).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn provider_api_key_ignores_environment_variables() {
        unsafe {
            std::env::set_var("DASHSCOPE_API_KEY", "env-secret");
        }
        let provider = RelayProviderRecord {
            id: "dashscope".into(),
            name: "DashScope".into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            api_key: None,
            env_key: Some("DASHSCOPE_API_KEY".into()),
            model: "qwen3.7-max".into(),
            wire_api: "responses".into(),
            models_sample: Vec::new(),
            last_error: None,
        };

        assert_eq!(provider_api_key(&provider), None);
        unsafe {
        std::env::remove_var("DASHSCOPE_API_KEY");
        }
    }

    #[test]
    fn relay_server_spawn_does_not_require_existing_tokio_runtime() {
        let (paths, root) = paths("spawn-without-runtime");

        let handle = spawn_relay_proxy_server_at(Arc::new(paths), "not-a-socket-address");

        handle.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activate_provider_writes_stable_router_provider_and_catalog() {
        let (paths, root) = paths("activate");
        fs::write(
            &paths.config_path,
            "model = \"gpt-5.5\"\nmodel_provider = \"openai\"\n\n[mcp_servers.context7]\ncommand = \"npx\"\n",
        )
        .unwrap();
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();

        activate_relay_provider(&paths, "dashscope").unwrap();

        let config = fs::read_to_string(&paths.config_path).unwrap();
        assert!(config.contains("[mcp_servers.context7]"));
        assert!(config.contains("# --- AiMaMi Relay Managed Block (top) ---"));
        assert!(config.contains("# --- AiMaMi Relay Managed Block (providers) ---"));
        assert!(config.contains("model_provider = \"custom\""));
        assert!(config.contains("model = \"gpt-5.5\""));
        assert!(config.contains("model_catalog_json = \"codex_router_catalog.json\""));
        assert!(!config.contains("model_provider = \"aimami_dashscope\""));
        assert!(config.contains("[model_providers.custom]"));
        assert!(config.contains("base_url = \"http://127.0.0.1:49735/v1\""));
        assert!(config.contains("requires_openai_auth = true"));
        assert!(config.contains("supports_websockets = false"));
        assert!(!config.contains("api_key = \"dashscope-key\""));
        let top_pos = config.find(RELAY_TOP_MANAGED_BLOCK_BEGIN).unwrap();
        let mcp_pos = config.find("[mcp_servers.context7]").unwrap();
        assert!(top_pos < mcp_pos);

        let parsed: toml::Value = config.parse().unwrap();
        assert_eq!(parsed["model_provider"].as_str(), Some("custom"));
        assert_eq!(
            parsed["model_catalog_json"].as_str(),
            Some("codex_router_catalog.json")
        );
        assert_eq!(parsed["model"].as_str(), Some("gpt-5.5"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activate_dashscope_writes_router_catalog_without_per_provider_switch() {
        let (paths, root) = paths("dashscope");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();

        activate_relay_provider(&paths, "dashscope").unwrap();

        let config = fs::read_to_string(&paths.config_path).unwrap();
        assert!(!config.contains("model_provider = \"aimami_dashscope\""));
        assert!(!config.contains("model = \"qwen3.7-max\""));
        assert!(config.contains("model_provider = \"custom\""));
        assert!(config.contains("model_catalog_json = \"codex_router_catalog.json\""));
        assert!(!config.contains("env_key = \"DASHSCOPE_API_KEY\""));
        assert!(config.contains("wire_api = \"responses\""));
        assert!(config.contains("requires_openai_auth = true"));
        assert!(config.contains("supports_websockets = false"));
        assert!(!config.contains("api_key ="));
        assert!(!config.contains("[model_providers.aimami_dashscope]"));
        let parsed: toml::Value = config.parse().unwrap();
        assert_eq!(parsed["model_provider"].as_str(), Some("custom"));
        assert_eq!(
            parsed["model_catalog_json"].as_str(),
            Some("codex_router_catalog.json")
        );

        let catalog_path = relay_model_catalog_path(&paths);
        let catalog: Value =
            serde_json::from_str(&fs::read_to_string(catalog_path).unwrap()).unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert!(models.iter().any(|model| model["slug"] == "gpt-5.5"));
        assert!(models.iter().any(|model| model["slug"] == "qwen3.7-max"));
        assert!(models.iter().any(|model| model["slug"] == "glm-5.2"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activate_provider_auto_syncs_session_provider_buckets_without_backups() {
        let (paths, root) = paths("activate-session-bucket");
        fs::create_dir_all(&paths.sessions_dir).unwrap();
        let openai_session = paths.sessions_dir.join("openai.jsonl");
        let legacy_aimami_session = paths.sessions_dir.join("aimami.jsonl");
        let legacy_vendor_session = paths.sessions_dir.join("deepseek.jsonl");
        fs::write(
            &openai_session,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"openai-session","model_provider":"openai"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"text":"hello"}}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            &legacy_aimami_session,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"aimami-session","model_provider":"aimami"}}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            &legacy_vendor_session,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"deepseek-session","model_provider":"deepseek"}}"#,
                "\n"
            ),
        )
        .unwrap();
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();

        activate_relay_provider(&paths, "dashscope").unwrap();

        let openai_raw = fs::read_to_string(openai_session).unwrap();
        let legacy_raw = fs::read_to_string(legacy_aimami_session).unwrap();
        let vendor_raw = fs::read_to_string(legacy_vendor_session).unwrap();
        assert!(openai_raw.contains(r#""model_provider":"custom""#));
        assert!(legacy_raw.contains(r#""model_provider":"custom""#));
        assert!(vendor_raw.contains(r#""model_provider":"custom""#));
        assert!(openai_raw.contains(r#""type":"response_item""#));

        let ledger_dir = paths.codexmate_dir.join("session-provider-auto-sync-ledgers");
        let backup_dir = paths.codexmate_dir.join("session-provider-migration-backups");
        let ledgers = fs::read_dir(&ledger_dir)
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(ledgers.len(), 1);
        let ledger_raw = fs::read_to_string(ledgers[0].path()).unwrap();
        assert!(ledger_raw.contains(r#""targetModelProvider": "custom""#));
        assert!(ledger_raw.contains(r#""originalModelProvider": "openai""#));
        assert!(ledger_raw.contains(r#""originalModelProvider": "aimami""#));
        assert!(ledger_raw.contains(r#""originalModelProvider": "deepseek""#));
        assert!(!backup_dir.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activate_provider_prefers_codex_models_cache_for_official_catalog() {
        let (paths, root) = paths("models-cache");
        fs::write(
            paths.codex_home.join("models_cache.json"),
            r#"{
  "models": [
    { "slug": "gpt-cache-official", "display_name": "GPT Cache Official", "context_window": 256000 },
    { "slug": "gpt-cache-mini", "display_name": "GPT Cache Mini", "context_window": 128000 }
  ]
}"#,
        )
        .unwrap();
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();

        activate_relay_provider(&paths, "dashscope").unwrap();

        let catalog: Value =
            serde_json::from_str(&fs::read_to_string(relay_model_catalog_path(&paths)).unwrap())
                .unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert!(models.iter().any(|model| model["slug"] == "gpt-cache-official"));
        assert!(models.iter().any(|model| model["slug"] == "gpt-cache-mini"));
        assert!(models.iter().any(|model| model["slug"] == "qwen3.7-max"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_codex_bundled_models_accepts_pretty_json_stdout() {
        let output = r#"{
  "models": [
    { "slug": "gpt-5.5", "display_name": "GPT-5.5" },
    { "slug": "gpt-5.4-mini", "display_name": "GPT-5.4 Mini" }
  ]
}"#;

        let models = parse_codex_bundled_models_output(output).unwrap();

        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["slug"], "gpt-5.5");
        assert_eq!(models[1]["slug"], "gpt-5.4-mini");
    }

    #[test]
    fn provider_catalog_models_clone_codex_template_shape() {
        let provider = RelayProviderRecord {
            id: "dashscope".into(),
            name: "DashScope".into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            api_key: Some("dashscope-key".into()),
            env_key: None,
            model: "qwen3.7-max".into(),
            wire_api: "responses".into(),
            models_sample: Vec::new(),
            last_error: None,
        };
        let template = serde_json::json!({
            "slug": "gpt-5.5",
            "display_name": "GPT-5.5",
            "description": "Official template",
            "context_window": 400000,
            "max_context_window": 400000,
            "priority": 10,
            "service_tiers": [{ "id": "openai" }],
            "additional_speed_tiers": [{ "id": "fast" }],
            "availability_nux": { "message": "upgrade" },
            "upgrade": { "plan": "pro" },
            "supports_reasoning_summaries": true,
            "default_reasoning_summary": "auto",
            "support_verbosity": true,
            "default_verbosity": "low",
            "template_only_field": "keep"
        });

        let models = provider_catalog_models(&provider, Some(&template));
        let qwen = models
            .iter()
            .find(|model| model["slug"] == "qwen3.7-max")
            .unwrap();

        assert_eq!(qwen["display_name"], "Qwen3.7 Max");
        assert_eq!(qwen["description"], "DashScope model");
        assert_eq!(qwen["context_window"], 400000);
        assert_eq!(qwen["max_context_window"], 400000);
        assert_eq!(qwen["template_only_field"], "keep");
        assert_eq!(qwen["service_tiers"], serde_json::json!([]));
        assert_eq!(qwen["additional_speed_tiers"], serde_json::json!([]));
        assert!(qwen["availability_nux"].is_null());
        assert!(qwen["upgrade"].is_null());
        assert_eq!(qwen["supports_reasoning_summaries"], false);
        assert_eq!(qwen["default_reasoning_summary"], "none");
        assert_eq!(qwen["support_verbosity"], false);
        assert!(qwen["default_verbosity"].is_null());
    }

    #[test]
    fn catalog_glm_model_context_window_is_one_million() {
        let provider = RelayProviderRecord {
            id: "dashscope".into(),
            name: "DashScope".into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            api_key: Some("dashscope-key".into()),
            env_key: None,
            model: "glm-5.2".into(),
            wire_api: "responses".into(),
            models_sample: vec!["glm-5.2".into()],
            last_error: None,
        };
        let template = serde_json::json!({
            "slug": "gpt-5.5",
            "context_window": 272000,
            "max_context_window": 272000,
        });
        let models = provider_catalog_models(&provider, Some(&template));
        let glm = models
            .iter()
            .find(|m| m["slug"] == "glm-5.2")
            .unwrap();
        assert_eq!(glm["context_window"], 1_048_576);
        assert_eq!(glm["max_context_window"], 1_048_576);

        // Non-glm models keep template context_window
        let qwen_provider = RelayProviderRecord {
            id: "dashscope".into(),
            name: "DashScope".into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            api_key: Some("dashscope-key".into()),
            env_key: None,
            model: "qwen3.7-max".into(),
            wire_api: "responses".into(),
            models_sample: vec!["qwen3.7-max".into()],
            last_error: None,
        };
        let qwen_models = provider_catalog_models(&qwen_provider, Some(&template));
        let qwen = qwen_models
            .iter()
            .find(|m| m["slug"] == "qwen3.7-max")
            .unwrap();
        assert_eq!(qwen["context_window"], 272000);
        assert_eq!(qwen["max_context_window"], 272000);
    }

    #[test]
    fn deleting_active_provider_restores_pre_relay_config() {
        let (paths, root) = paths("restore-config");
        let original_config = r#"model = "gpt-5.5"
model_provider = "custom"
model_catalog_json = "my-catalog.json"

[model_providers.custom]
name = "Custom"
base_url = "https://custom.example/v1"
wire_api = "responses"
"#;
        fs::write(&paths.config_path, original_config).unwrap();
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        delete_relay_provider(&paths, "dashscope").unwrap();

        assert_eq!(fs::read_to_string(&paths.config_path).unwrap(), original_config);
        assert!(!relay_config_backup_path(&paths).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_relay_state_reports_config_diagnostics() {
        let (paths, root) = paths("diagnostics");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();

        let inactive = load_relay_state(&paths).unwrap();
        assert!(inactive.diagnostics.registry_exists);
        assert!(!inactive.diagnostics.config_exists);
        assert!(!inactive.diagnostics.managed_block_present);
        assert!(!inactive.diagnostics.active_provider_configured);

        activate_relay_provider(&paths, "dashscope").unwrap();

        let active = load_relay_state(&paths).unwrap();
        assert!(active.diagnostics.registry_exists);
        assert!(active.diagnostics.config_exists);
        assert!(active.diagnostics.managed_block_present);
        assert!(active.diagnostics.active_provider_configured);
        assert!(!active.diagnostics.relay_server_reachable);
        assert_eq!(active.diagnostics.issue_message, None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn relay_reachable_probe_rejects_non_aimami_service() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"service":"other"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("relay-health-other-service");
        assert!(!relay_server_reachable_at(
            &paths,
            addr,
            Duration::from_secs(1)
        ));
        handle.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn relay_reachable_probe_rejects_wrong_codex_home() {
        let (paths, root) = paths("relay-health-wrong-home");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"service":"aimami-relay","ok":true,"codexHome":"/tmp/other-codex"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        assert!(!relay_server_reachable_at(
            &paths,
            addr,
            Duration::from_secs(1)
        ));
        handle.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_health_endpoint_identifies_aimami_relay() {
        let (paths, root) = paths("relay-health");

        let response = relay_http_response_for_request(
            &paths,
            b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .await
        .unwrap();

        assert_eq!(response.status_code, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["service"], "aimami-relay");
        assert_eq!(body["ok"], true);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dashscope_catalog_models_do_not_depend_on_provider_id() {
        let provider = RelayProviderRecord {
            id: "qwen3_7_max".into(),
            name: "Qwen3.7 Max".into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            api_key: Some("dashscope-key".into()),
            env_key: None,
            model: "qwen3.7-max".into(),
            wire_api: "responses".into(),
            models_sample: Vec::new(),
            last_error: None,
        };

        let slugs = provider_model_slugs(&provider);
        assert!(slugs.contains(&"qwen3.7-max".to_string()));
        assert!(slugs.contains(&"qwen3.7-plus".to_string()));
        assert!(slugs.contains(&"glm-5.2".to_string()));
    }

    #[test]
    fn dashscope_catalog_models_include_only_selected_custom_models() {
        let provider = RelayProviderRecord {
            id: "dashscope".into(),
            name: "DashScope".into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            api_key: Some("dashscope-key".into()),
            env_key: None,
            model: "qwen3.7-max".into(),
            wire_api: "responses".into(),
            models_sample: Vec::new(),
            last_error: None,
        };

        let slugs = provider_model_slugs(&provider);
        assert_eq!(slugs, vec!["qwen3.7-max", "qwen3.7-plus", "glm-5.2"]);
    }

    #[test]
    fn responses_to_chat_request_maps_tools_calls_and_outputs() {
        let payload = serde_json::json!({
            "model": "glm-5.2",
            "stream": true,
            "parallel_tool_calls": true,
            "tools": [{
                "type": "function",
                "name": "exec_command",
                "description": "Run a shell command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "cmd": { "type": "string" }
                    },
                    "required": ["cmd"]
                }
            }],
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "commit 修改" }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"git status --short\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": " M src-tauri/src/core/relay.rs\n"
                }
            ]
        });

        let chat = responses_to_chat_request_body(&payload).unwrap();
        let messages = chat["messages"].as_array().unwrap();

        assert_eq!(chat["tools"][0]["type"], "function");
        assert_eq!(chat["tools"][0]["function"]["name"], "exec_command");
        assert_eq!(chat["parallel_tool_calls"], true);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "commit 修改");
        assert_eq!(messages[1]["role"], "assistant");
        assert!(messages[1]["content"].is_null());
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["arguments"],
            "{\"cmd\":\"git status --short\"}"
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_1");
        assert_eq!(messages[2]["content"], " M src-tauri/src/core/relay.rs\n");
    }

    #[test]
    fn responses_to_chat_request_maps_single_input_object() {
        let payload = serde_json::json!({
            "model": "glm-5.2",
            "input": {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "done"
            }
        });

        let chat = responses_to_chat_request_body(&payload).unwrap();
        let messages = chat["messages"].as_array().unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_1");
        assert_eq!(messages[0]["content"], "done");
    }

    #[test]
    fn responses_to_chat_request_maps_reasoning_effort_for_glm() {
        let payload_high = serde_json::json!({
            "model": "glm-5.2",
            "input": "hi",
            "reasoning_effort": "high"
        });
        let chat = responses_to_chat_request_body(&payload_high).unwrap();
        assert_eq!(chat["reasoning_effort"], "high");

        let payload_xhigh = serde_json::json!({
            "model": "glm-5.2",
            "input": "hi",
            "reasoning_effort": "xhigh"
        });
        let chat = responses_to_chat_request_body(&payload_xhigh).unwrap();
        assert_eq!(chat["reasoning_effort"], "high");

        let payload_medium = serde_json::json!({
            "model": "glm-5.2",
            "input": "hi",
            "reasoning_effort": "medium"
        });
        let chat = responses_to_chat_request_body(&payload_medium).unwrap();
        assert_eq!(chat["reasoning_effort"], "low");
        assert!(chat.get("enable_thinking").is_none());

        let payload_low = serde_json::json!({
            "model": "glm-5.2",
            "input": "hi",
            "reasoning_effort": "low"
        });
        let chat = responses_to_chat_request_body(&payload_low).unwrap();
        assert_eq!(chat["enable_thinking"], false);
        assert!(chat.get("reasoning_effort").is_none());
    }

    #[test]
    fn chat_json_to_responses_json_maps_tool_calls() {
        let chat = serde_json::json!({
            "id": "chatcmpl_tool",
            "object": "chat.completion",
            "created": 1,
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "exec_command",
                            "arguments": "{\"cmd\":\"git status --short\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let output = chat_json_to_responses_json(chat.to_string().as_bytes(), "glm-5.2").unwrap();
        let response: Value = serde_json::from_slice(&output).unwrap();

        assert_eq!(response["output"][0]["type"], "function_call");
        assert_eq!(response["output"][0]["call_id"], "call_1");
        assert_eq!(response["output"][0]["name"], "exec_command");
        assert_eq!(
            response["output"][0]["arguments"],
            "{\"cmd\":\"git status --short\"}"
        );
    }

    #[test]
    fn chat_sse_to_responses_sse_streams_tool_calls_without_empty_text_item() {
        let first = serde_json::json!({
            "id": "chatcmpl_tool_stream",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "exec_command",
                            "arguments": "{\"cmd\""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let second = serde_json::json!({
            "id": "chatcmpl_tool_stream",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": ":\"git status --short\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let body = format!("data: {first}\n\ndata: {second}\n\ndata: [DONE]\n\n");

        let output = chat_sse_to_responses_sse(body.as_bytes(), "glm-5.2").unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("event: response.output_item.added"));
        assert!(output.contains(r#""type":"function_call""#));
        assert!(output.contains(r#""call_id":"call_1""#));
        assert!(output.contains(r#""name":"exec_command""#));
        assert!(output.contains("event: response.function_call_arguments.delta"));
        assert!(output.contains("event: response.function_call_arguments.done"));
        assert!(output.contains("event: response.output_item.done"));
        assert!(output.contains("event: response.completed"));
        assert!(!output.contains("event: response.output_text.done"));
    }

    #[test]
    fn official_models_default_to_codex_backend_responses_endpoint() {
        assert_eq!(
            RelayResponsesEndpoint::Responses.openai_url(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            RelayResponsesEndpoint::Compact.openai_url(),
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
    }

    #[tokio::test]
    async fn relay_reachable_probe_accepts_current_aimami_relay() {
        let (paths, root) = paths("relay-health-current-home");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let relay_paths = Arc::new(paths.clone());
        let relay_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            serve_relay_connection(
                relay_paths,
                stream,
                "http://127.0.0.1:9/v1/responses".to_string(),
            )
            .await
            .unwrap();
        });

        let probe_paths = paths.clone();
        let reachable = tokio::task::spawn_blocking(move || {
            relay_server_reachable_at(&probe_paths, addr, Duration::from_secs(1))
        })
        .await
        .unwrap();

        assert!(reachable);
        let _ = relay_handle.await;
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_relay_draft_rejects_non_responses_wire_api() {
        let error = test_relay_draft(RelayProviderDraftPayload {
            id: None,
            name: "Local".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: Some("test-key".into()),
            model: "model-a".into(),
            wire_api: "openai-chat".into(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("Responses wire API"));
    }

    #[test]
    fn test_relay_draft_posts_to_responses_for_responses_wire_api() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/responses "));
            assert!(request.contains("\"model\":\"qwen3.7-max\""));
            assert!(request.contains("\"input\":\"ping\""));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer dashscope-key"));
            let body = r#"{"id":"resp_test","object":"response","status":"completed","model":"qwen3.7-max","output":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let result = test_relay_draft(RelayProviderDraftPayload {
            id: Some("dashscope".into()),
            name: "DashScope".into(),
            base_url: format!("http://{addr}/v1"),
            api_key: Some("dashscope-key".into()),
            model: "qwen3.7-max".into(),
            wire_api: "responses".into(),
        })
        .unwrap();

        assert!(result.ok);
        assert_eq!(result.status_code, Some(200));
        assert_eq!(result.models, vec!["qwen3.7-max"]);
        handle.join().unwrap();
    }

    #[tokio::test]
    async fn relay_models_endpoint_returns_merged_catalog() {
        let (paths, root) = paths("proxy-models");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let response = relay_http_response_for_request(
            &paths,
            b"GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .await
        .unwrap();

        assert_eq!(response.status_code, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        let models = body["models"].as_array().unwrap();
        assert!(models.iter().any(|model| model["slug"] == "gpt-5.5"));
        assert!(models.iter().any(|model| model["slug"] == "qwen3.7-max"));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_models_endpoint_accepts_codex_path_variants() {
        let (paths, root) = paths("proxy-model-path-variants");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        for path in ["/v1/v1/models", "/codex/v1/models"] {
            let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
            let response = relay_http_response_for_request(&paths, request.as_bytes())
                .await
                .unwrap();
            assert_eq!(response.status_code, 200, "path {path}");
            let body: Value = serde_json::from_slice(&response.body).unwrap();
            let models = body["models"].as_array().unwrap();
            assert!(
                models.iter().any(|model| model["slug"] == "qwen3.7-max"),
                "path {path}"
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_models_endpoint_hides_catalog_when_config_points_elsewhere() {
        let (paths, root) = paths("proxy-models-stale-config");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();
        fs::write(
            &paths.config_path,
            "model_provider = \"openai\"\nmodel_catalog_json = \"someone-else.json\"\n",
        )
        .unwrap();

        let response = relay_http_response_for_request(
            &paths,
            b"GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .await
        .unwrap();

        assert_eq!(response.status_code, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        let models = body["models"].as_array().unwrap();
        assert!(models.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_responses_endpoint_accepts_double_v1_path_for_qwen() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/chat/completions "));
            assert!(request.contains("\"model\":\"qwen3.7-max\""));
            let body = r#"{"id":"chatcmpl_test","object":"chat.completion","created":1,"model":"qwen3.7-max","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("proxy-responses-double-v1");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"qwen3.7-max","input":"hello"}"#;
        let request = format!(
            "POST /v1/v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = relay_http_response_for_request(&paths, request.as_bytes())
            .await
            .unwrap();

        assert_eq!(response.status_code, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["model"], "qwen3.7-max");
        assert_eq!(body["output"][0]["content"][0]["text"], "OK");
        handle.join().unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_responses_endpoint_forwards_qwen_to_active_provider() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/chat/completions "));
            assert!(request.contains("\"model\":\"qwen3.7-max\""));
            assert!(request.contains("\"messages\":[{\"content\":\"hello\",\"role\":\"user\"}]"));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer dashscope-key"));
            let body = r#"{"id":"chatcmpl_test","object":"chat.completion","created":1,"model":"qwen3.7-max","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("proxy-responses");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"qwen3.7-max","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = relay_http_response_for_request(&paths, request.as_bytes())
            .await
            .unwrap();

        assert_eq!(response.status_code, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["model"], "qwen3.7-max");
        assert_eq!(body["output"][0]["content"][0]["text"], "OK");
        handle.join().unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_provider_retries_429_with_backoff() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for attempt in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(request.starts_with("POST /v1/chat/completions "));
                if attempt < 2 {
                    let body = r#"{"error":{"message":"rate limited","type":"rate_limit"}}"#;
                    let response = format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                } else {
                    let body = r#"{"id":"chatcmpl_retry","object":"chat.completion","created":1,"model":"glm-5.2","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}]}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            }
        });

        let (paths, root) = paths("provider-retry-429");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "glm-5.2".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"glm-5.2","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = relay_http_response_for_request(&paths, request.as_bytes())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&response.body).unwrap();

        assert_eq!(response.status_code, 200);
        assert_eq!(body["output"][0]["content"][0]["text"], "OK");
        handle.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relay_provider_smooths_concurrent_requests() {
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let arrivals = Arc::new(Mutex::new(Vec::new()));
        let server_arrivals = Arc::clone(&arrivals);
        let handle = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                server_arrivals.lock().unwrap().push(Instant::now());
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(request.starts_with("POST /v1/chat/completions "));
                let body = r#"{"id":"chatcmpl_smooth","object":"chat.completion","created":1,"model":"glm-5.2","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let (paths, root) = paths("provider-smooth-concurrency");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "glm-5.2".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"glm-5.2","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let first = relay_http_response_for_request(&paths, request.as_bytes());
        let second = relay_http_response_for_request(&paths, request.as_bytes());
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first.unwrap().status_code, 200);
        assert_eq!(second.unwrap().status_code, 200);
        handle.join().unwrap();
        let arrivals = arrivals.lock().unwrap();
        assert_eq!(arrivals.len(), 2);
        assert!(arrivals[1].duration_since(arrivals[0]) >= Duration::from_millis(900));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relay_provider_allows_concurrent_requests_under_limit() {
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let arrivals = Arc::new(Mutex::new(Vec::new()));
        let server_arrivals = Arc::clone(&arrivals);
        let handle = thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                server_arrivals.lock().unwrap().push(Instant::now());
                handlers.push(thread::spawn(move || {
                    let mut request = [0_u8; 4096];
                    let _ = stream.read(&mut request).unwrap();
                    let body = r#"{"id":"chatcmpl_concurrent","object":"chat.completion","created":1,"model":"glm-5.2","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}]}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }));
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });

        let (paths, root) = paths("provider-concurrent-under-limit");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "glm-5.2".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"glm-5.2","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let first = relay_http_response_for_request(&paths, request.as_bytes());
        let second = relay_http_response_for_request(&paths, request.as_bytes());
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first.unwrap().status_code, 200);
        assert_eq!(second.unwrap().status_code, 200);
        handle.join().unwrap();
        let arrivals = arrivals.lock().unwrap();
        assert_eq!(arrivals.len(), 2);
        // With concurrency=20, two requests can be in-flight simultaneously.
        // Rate slot still spaces request starts by ~1s, but both arrive
        // well under the ~3s it would take if serialized end-to-end.
        assert!(arrivals[1].duration_since(arrivals[0]) < Duration::from_secs(2));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_provider_retries_429_respecting_retry_after_header() {
        use std::time::Instant;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for attempt in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(request.starts_with("POST /v1/chat/completions "));
                if attempt < 2 {
                    let body = r#"{"error":{"message":"rate limited","type":"rate_limit"}}"#;
                    // Retry-After: 1 means the relay should wait ~1s, not the default exponential delay
                    let response = format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 1\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                } else {
                    let body = r#"{"id":"chatcmpl_retry_after","object":"chat.completion","created":1,"model":"glm-5.2","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}]}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            }
        });

        let (paths, root) = paths("provider-retry-after");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "glm-5.2".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"glm-5.2","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let start = Instant::now();
        let response = relay_http_response_for_request(&paths, request.as_bytes())
            .await
            .unwrap();
        let elapsed = start.elapsed();
        let body: Value = serde_json::from_slice(&response.body).unwrap();

        assert_eq!(response.status_code, 200);
        assert_eq!(body["output"][0]["content"][0]["text"], "OK");
        // Two retries with Retry-After: 1 each => at least ~2s total wait
        assert!(elapsed >= Duration::from_secs(2));
        handle.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_provider_compact_requests_use_standard_responses_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/chat/completions "));
            assert!(request.contains("\"model\":\"qwen3.7-max\""));
            let chunk = serde_json::json!({
                "id": "chatcmpl_compact",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "qwen3.7-max",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "OK" },
                    "finish_reason": "stop"
                }]
            });
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("proxy-provider-compact");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"qwen3.7-max","input":"hello","stream":true}"#;
        let request = format!(
            "POST /v1/responses/compact HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = relay_http_response_for_request(&paths, request.as_bytes())
            .await
            .unwrap();

        assert_eq!(response.status_code, 200);
        assert!(String::from_utf8_lossy(&response.body).contains("response.completed"));
        handle.join().unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn dashscope_chat_adapter_normalizes_chat_sse_to_responses_events() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/chat/completions "));
            assert!(request.contains("\"model\":\"qwen3.7-max\""));
            let chunk = serde_json::json!({
                "id": "chatcmpl_normalize",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "qwen3.7-max",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "OK" },
                    "finish_reason": "stop"
                }]
            });
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("normalize-provider-events");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"qwen3.7-max","input":"hello","stream":true}"#;
        let request = format!(
            "POST /v1/responses/compact HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = relay_http_response_for_request(&paths, request.as_bytes())
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&response.body);

        assert_eq!(response.status_code, 200);
        assert!(body.contains("response.completed"));
        assert!(body.contains("response.output_text.delta"));
        assert!(!body.contains("chat.completion.chunk"));
        handle.join().unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn dashscope_streaming_requests_use_chat_completions_adapter() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/chat/completions "));
            assert!(request.contains("\"model\":\"qwen3.7-max\""));
            assert!(request.contains("\"messages\":[{\"content\":\"hello\",\"role\":\"user\"}]"));
            assert!(request.contains("\"stream\":true"));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer dashscope-key"));

            let first = serde_json::json!({
                "id": "chatcmpl_test",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "qwen3.7-max",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant", "content": "OK" },
                    "finish_reason": null
                }]
            });
            let done = serde_json::json!({
                "id": "chatcmpl_test",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "qwen3.7-max",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3 }
            });
            let body = format!("data: {first}\n\ndata: {done}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("dashscope-chat-adapter");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let body = r#"{"model":"qwen3.7-max","input":"hello","stream":true}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = relay_http_response_for_request(&paths, request.as_bytes())
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&response.body);

        assert_eq!(response.status_code, 200);
        assert_eq!(response.content_type, "text/event-stream");
        assert!(body.contains("event: response.created"));
        assert!(body.contains("event: response.output_text.delta"));
        assert!(body.contains(r#""delta":"OK""#));
        assert!(body.contains("event: response.completed"));
        assert!(!body.contains("chat.completion.chunk"));
        handle.join().unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sse_event_normalizer_handles_event_names_split_across_chunks() {
        let mut normalizer = SseEventNormalizer::default();
        let mut output = Vec::new();
        output.extend(normalizer.push(b"event:response-com"));
        output.extend(normalizer.push(b"pleted\ndata:{\"type\":\"response-com"));
        output.extend(normalizer.push(b"pleted\"}\n\n"));
        output.extend(normalizer.finish());

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("event:response.completed"));
        assert!(output.contains(r#""type":"response.completed""#));
        assert!(!output.contains("response-completed"));
    }

    #[test]
    fn sse_event_normalizer_filters_dashscope_reasoning_for_desktop() {
        let mut normalizer = SseEventNormalizer::default();
        let input = concat!(
            "id:1\n",
            "event:response-reasoning_summary_text-delta\n",
            ":HTTP_STATUS/200\n",
            "data:{\"type\":\"response-reasoning_summary_text-delta\",\"output_index\":0,\"delta\":\"thinking\"}\n\n",
            "id:2\n",
            "event:response-output_item-added\n",
            ":HTTP_STATUS/200\n",
            "data:{\"type\":\"response-output_item-added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"r1\"}}\n\n",
            "id:3\n",
            "event:response-output_text-delta\n",
            ":HTTP_STATUS/200\n",
            "data:{\"type\":\"response-output_text-delta\",\"output_index\":1,\"delta\":\"OK\"}\n\n",
            "id:4\n",
            "event:response-completed\n",
            ":HTTP_STATUS/200\n",
            "data:{\"type\":\"response-completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"reasoning\",\"id\":\"r1\"},{\"type\":\"message\",\"id\":\"m1\"}]}}\n\n",
        );

        let mut output = normalizer.push(input.as_bytes());
        output.extend(normalizer.finish());

        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("HTTP_STATUS"));
        assert!(!output.contains("reasoning_summary_text"));
        assert!(!output.contains(r#""type":"reasoning""#));
        assert!(output.contains("event:response.output_text.delta"));
        assert!(output.contains(r#""output_index":0"#));
        assert!(!output.contains(r#""output_index":1"#));
        assert!(output.contains("event:response.completed"));
        assert!(output.contains(r#""type":"message""#));
    }

    #[test]
    fn provider_sse_normalization_is_limited_to_dashscope() {
        let provider = RelayProviderRecord {
            id: "openai-compatible".into(),
            name: "OpenAI Compatible".into(),
            base_url: "https://proxy.example.com/v1".into(),
            api_key: Some("provider-key".into()),
            env_key: None,
            model: "gpt-5.5".into(),
            wire_api: "responses".into(),
            models_sample: Vec::new(),
            last_error: None,
        };
        let body = concat!(
            "event:response-reasoning_summary_text-delta\r\n",
            ":HTTP_STATUS/200\r\n",
            "data:{\"type\":\"response-reasoning_summary_text-delta\",\"delta\":\"thinking\"}\r\n\r\n"
        )
        .as_bytes()
        .to_vec();

        let output = normalize_provider_sse_body(&provider, "text/event-stream", body.clone());

        assert_eq!(output, body);
    }

    #[tokio::test]
    async fn relay_responses_endpoint_passes_official_model_to_openai_with_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/responses "));
            assert!(request.contains("\"model\":\"gpt-5.5\""));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer openai-token"));
            let body = r#"{"id":"resp_openai","object":"response","status":"completed","model":"gpt-5.5","output":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("proxy-openai");
        let body = r#"{"model":"gpt-5.5","input":"hello"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer openai-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = relay_responses_response(
            &paths,
            &parse_http_request(request.as_bytes()).unwrap().headers,
            body.as_bytes(),
            RelayResponsesEndpoint::Responses,
            &format!("http://{addr}/v1/responses"),
        )
        .await
        .unwrap();

        assert_eq!(response.status_code, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["model"], "gpt-5.5");
        handle.join().unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_responses_compact_endpoint_passes_official_model_to_openai_with_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/responses/compact "));
            assert!(request.contains("\"model\":\"gpt-5.5\""));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer openai-token"));
            let body = r#"{"id":"resp_compact","object":"response","status":"completed","model":"gpt-5.5","output":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("proxy-openai-compact");
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        let relay_paths = Arc::new(paths.clone());
        let relay_handle = tokio::spawn(async move {
            let (stream, _) = relay_listener.accept().await.unwrap();
            serve_relay_connection(
                relay_paths,
                stream,
                format!("http://{addr}/v1/responses"),
            )
            .await
            .unwrap();
        });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut client = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
        let body = r#"{"model":"gpt-5.5","input":"compact"}"#;
        let request = format!(
            "POST /v1/responses/compact HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer openai-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        client.write_all(request.as_bytes()).await.unwrap();

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains(r#""model":"gpt-5.5""#));
        let _ = relay_handle.await;
        handle.join().unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_streams_upstream_sse_before_completion() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let upstream_handle = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/chat/completions "));
            assert!(request.contains("\"model\":\"qwen3.7-max\""));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer dashscope-key"));
            let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
            stream.write_all(headers.as_bytes()).unwrap();
            let first = serde_json::json!({
                "id": "chatcmpl_stream",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "qwen3.7-max",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "one" },
                    "finish_reason": null
                }]
            });
            let first = format!("data: {first}\n\n");
            write!(stream, "{:x}\r\n{}\r\n", first.len(), first).unwrap();
            stream.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(600));
            let second = serde_json::json!({
                "id": "chatcmpl_stream",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "qwen3.7-max",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "two" },
                    "finish_reason": "stop"
                }]
            });
            let second = format!("data: {second}\n\ndata: [DONE]\n\n");
            write!(stream, "{:x}\r\n{}\r\n0\r\n\r\n", second.len(), second).unwrap();
        });

        let (paths, root) = paths("stream-sse");
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: format!("http://{upstream_addr}/v1"),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        let relay_paths = Arc::new(paths.clone());
        let relay_handle = tokio::spawn(async move {
            let (stream, _) = relay_listener.accept().await.unwrap();
            serve_relay_connection(
                relay_paths,
                stream,
                "http://127.0.0.1:9/v1/responses".to_string(),
            )
                .await
                .unwrap();
        });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut client = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
        let body = r#"{"model":"qwen3.7-max","input":"hello","stream":true}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer openai-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        client.write_all(request.as_bytes()).await.unwrap();

        let mut observed = Vec::new();
        let first_chunk = tokio::time::timeout(std::time::Duration::from_millis(250), async {
            loop {
                let mut chunk = [0_u8; 128];
                let read = client.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                observed.extend_from_slice(&chunk[..read]);
                let observed = String::from_utf8_lossy(&observed);
                if observed.contains("response.output_text.delta") && observed.contains("one") {
                    break;
                }
            }
        })
        .await;
        assert!(
            first_chunk.is_ok(),
            "relay did not stream the first SSE chunk before upstream completion"
        );

        let _ = relay_handle.await;
        upstream_handle.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn relay_streaming_provider_requests_use_aimami_api_proxy() {
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let proxy_handle = thread::spawn(move || {
            let (mut stream, _) = proxy.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST http://provider.invalid/v1/chat/completions "));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer dashscope-key"));

            let chunk = serde_json::json!({
                "id": "chatcmpl_proxy",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "qwen3.7-max",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "OK" },
                    "finish_reason": "stop"
                }]
            });
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let (paths, root) = paths("proxy-stream");
        fs::write(
            &paths.settings_path,
            format!(
                r#"{{"apiProxy":{{"mode":"manual","url":"http://{proxy_addr}"}}}}"#
            ),
        )
        .unwrap();
        upsert_relay_provider(
            &paths,
            RelayProviderDraftPayload {
                id: Some("dashscope".into()),
                name: "DashScope".into(),
                base_url: "http://provider.invalid/v1".into(),
                api_key: Some("dashscope-key".into()),
                model: "qwen3.7-max".into(),
                wire_api: "responses".into(),
            },
        )
        .unwrap();
        activate_relay_provider(&paths, "dashscope").unwrap();

        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        let relay_paths = Arc::new(paths.clone());
        let relay_handle = tokio::spawn(async move {
            let (stream, _) = relay_listener.accept().await.unwrap();
            serve_relay_connection(
                relay_paths,
                stream,
                "http://127.0.0.1:9/v1/responses".to_string(),
            )
            .await
            .unwrap();
        });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut client = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
        let body = r#"{"model":"qwen3.7-max","input":"hello","stream":true}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer openai-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        client.write_all(request.as_bytes()).await.unwrap();

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("response.completed"));

        let _ = relay_handle.await;
        proxy_handle.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

}
