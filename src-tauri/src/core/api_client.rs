use crate::core::auth::ApiRequestContext;
use crate::core::models::{
    ApiProxyConfigPayload, ApiProxyDetectPayload, ApiProxyMode, ApiProxyTestPayload, CoreError,
};
use reqwest::blocking::Client;
use std::time::Duration;

const CONNECTIVITY_URL: &str = "https://chatgpt.com/";

pub fn sanitize_proxy_config(
    config: &ApiProxyConfigPayload,
) -> Result<ApiProxyConfigPayload, CoreError> {
    match config.mode {
        ApiProxyMode::Direct => Ok(ApiProxyConfigPayload {
            mode: ApiProxyMode::Direct,
            url: None,
        }),
        ApiProxyMode::Manual => {
            let url = config
                .url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    CoreError::InvalidData("Manual proxy mode requires a proxy URL".into())
                })?;
            if !(url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("socks5://"))
            {
                return Err(CoreError::InvalidData("Unsupported proxy scheme".into()));
            }
            Ok(ApiProxyConfigPayload {
                mode: ApiProxyMode::Manual,
                url: Some(url.to_string()),
            })
        }
    }
}

pub fn test_api_connectivity(
    config: &ApiProxyConfigPayload,
    context: Option<&ApiRequestContext>,
) -> ApiProxyTestPayload {
    let config = match sanitize_proxy_config(config) {
        Ok(config) => config,
        Err(error) => {
            return ApiProxyTestPayload {
                code: "invalid_config".into(),
                reachable: false,
                status_code: None,
                message: error.to_string(),
            };
        }
    };

    let mut builder = Client::builder().timeout(Duration::from_secs(5));
    if let Some(url) = config.url.as_deref() {
        match reqwest::Proxy::all(url) {
            Ok(proxy) => {
                builder = builder.proxy(proxy);
            }
            Err(error) => {
                return ApiProxyTestPayload {
                    code: "invalid_config".into(),
                    reachable: false,
                    status_code: None,
                    message: error.to_string(),
                };
            }
        }
    }

    let client = match builder.build() {
        Ok(client) => client,
        Err(error) => {
            return ApiProxyTestPayload {
                code: "client_build_failed".into(),
                reachable: false,
                status_code: None,
                message: error.to_string(),
            };
        }
    };

    let mut request = client.get(CONNECTIVITY_URL);
    if let Some(token) = context.and_then(|ctx| ctx.access_token.as_deref()) {
        request = request.bearer_auth(token);
    } else if let Some(api_key) = context.and_then(|ctx| ctx.api_key.as_deref()) {
        request = request.bearer_auth(api_key);
    }

    match request.send() {
        Ok(response) => ApiProxyTestPayload {
            code: "ok".into(),
            reachable: true,
            status_code: Some(i32::from(response.status().as_u16())),
            message: String::new(),
        },
        Err(error) => ApiProxyTestPayload {
            code: "network_error".into(),
            reachable: false,
            status_code: None,
            message: error.to_string(),
        },
    }
}

pub fn detect_api_proxy_config(context: Option<&ApiRequestContext>) -> ApiProxyDetectPayload {
    for url in crate::platform::proxy::detect_system_proxy_candidates() {
        let config = ApiProxyConfigPayload {
            mode: ApiProxyMode::Manual,
            url: Some(url.clone()),
        };
        let probe = test_api_connectivity(&config, context);
        if probe.reachable {
            return ApiProxyDetectPayload {
                found: true,
                mode: Some(ApiProxyMode::Manual),
                url: Some(url),
                probe,
            };
        }
    }

    ApiProxyDetectPayload {
        found: false,
        mode: None,
        url: None,
        probe: ApiProxyTestPayload {
            code: "not_found".into(),
            reachable: false,
            status_code: None,
            message: String::new(),
        },
    }
}
