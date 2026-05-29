//! Stdio JSON-RPC dispatch loop with outbound Slack RPC method support.
//!
//! The upstream `animus_plugin_runtime::trigger_backend_main` runtime only
//! dispatches the protocol-defined `trigger/*` methods — any unrecognized
//! method falls through to a `METHOD_NOT_FOUND` reply. To expose outbound
//! Slack Web API calls (`slack/chat_post_message`,
//! `slack/chat_post_ephemeral`, `slack/send_dm`) to workflow phases without
//! forking the protocol crates, this plugin owns its own dispatch loop and
//! handles the lifecycle + trigger methods identically, plus the three new
//! `slack/*` outbound methods.
//!
//! Wire shape stays 100% compatible with `trigger_backend_main`: lifecycle
//! methods (`initialize`, `initialized`, `$/ping`, `health/check`,
//! `shutdown`), `trigger/schema`, `trigger/watch`, `trigger/ack`,
//! `trigger/event` notifications.

use std::io::IsTerminal;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use animus_plugin_protocol::{
    error_codes, HealthStatus, InitializeResult, PluginCapabilities, PluginInfo, RpcError,
    RpcNotification, RpcRequest, RpcResponse, PROTOCOL_VERSION,
};
use animus_trigger_protocol::{
    BackendError as TriggerBackendError, TriggerBackend, METHOD_TRIGGER_ACK, METHOD_TRIGGER_SCHEMA,
    METHOD_TRIGGER_WATCH, NOTIFICATION_TRIGGER_EVENT,
};
use anyhow::Result;
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};
use tokio::sync::Mutex;

use crate::backend::SlackBackend;
use crate::web_api::{
    ChatPostEphemeralParams, ChatPostMessageParams, SendDmParams, SlackApiError, SlackWebClient,
};

/// `slack/chat_post_message` — wraps Slack's `chat.postMessage`.
pub const METHOD_SLACK_CHAT_POST_MESSAGE: &str = "slack/chat_post_message";
/// `slack/chat_post_ephemeral` — wraps Slack's `chat.postEphemeral`.
pub const METHOD_SLACK_CHAT_POST_EPHEMERAL: &str = "slack/chat_post_ephemeral";
/// `slack/send_dm` — opens an IM channel with the user then posts a message.
pub const METHOD_SLACK_SEND_DM: &str = "slack/send_dm";

/// Run the stdio JSON-RPC dispatch loop until stdin closes.
///
/// Mirrors `animus_plugin_runtime::trigger_backend_main` for the lifecycle +
/// trigger surface and adds dispatch arms for the three outbound Slack
/// methods. The `web` client is shared across all in-flight requests via
/// `Arc<SlackWebClient>` (the inner `reqwest::Client` already pools
/// connections).
pub async fn run(info: PluginInfo, backend: SlackBackend, web: SlackWebClient) -> Result<()> {
    refuse_terminal_stdin(&info.name);

    let capabilities = compute_capabilities(&backend);
    let backend = Arc::new(backend);
    let web = Arc::new(web);
    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut reader = BufReader::new(tokio::io::stdin());

    loop {
        let frame = match read_frame(&mut reader).await? {
            Some(frame) => frame,
            None => return Ok(()),
        };
        let info = info.clone();
        let capabilities = capabilities.clone();
        let backend = backend.clone();
        let web = web.clone();
        let stdout = stdout.clone();
        tokio::spawn(async move {
            handle_request(frame, info, capabilities, backend, web, stdout).await;
        });
    }
}

/// Capabilities advertised in the `initialize` response.
///
/// Includes the upstream trigger methods plus the three outbound `slack/*`
/// methods so hosts can detect support without trial-calling.
pub fn compute_capabilities<B: TriggerBackend>(backend: &B) -> PluginCapabilities {
    let schema = backend.schema();
    let mut methods = vec![
        METHOD_TRIGGER_WATCH.to_string(),
        METHOD_TRIGGER_SCHEMA.to_string(),
        "health/check".to_string(),
        METHOD_SLACK_CHAT_POST_MESSAGE.to_string(),
        METHOD_SLACK_CHAT_POST_EPHEMERAL.to_string(),
        METHOD_SLACK_SEND_DM.to_string(),
    ];
    if schema.supports_ack {
        methods.push(METHOD_TRIGGER_ACK.to_string());
    }
    PluginCapabilities {
        methods,
        streaming: true,
        progress: false,
        cancellation: false,
        subject_kinds: Vec::new(),
        mcp_tools: Vec::new(),
    }
}

#[derive(Debug, Deserialize)]
struct TriggerAckParams {
    event_id: String,
}

async fn handle_request(
    request: RpcRequest,
    info: PluginInfo,
    capabilities: PluginCapabilities,
    backend: Arc<SlackBackend>,
    web: Arc<SlackWebClient>,
    stdout: Arc<Mutex<Stdout>>,
) {
    let id = request.id.clone();
    let response = match request.method.as_str() {
        "initialize" => Some(initialize_response(id, &info, &capabilities)),
        "initialized" => None,
        "$/ping" => Some(RpcResponse::ok(id, json!({}))),
        "health/check" => Some(health_response(id, backend.health().await)),
        "shutdown" => Some(RpcResponse::ok(id, json!({}))),
        METHOD_TRIGGER_SCHEMA => Some(match serde_json::to_value(backend.schema()) {
            Ok(value) => RpcResponse::ok(id, value),
            Err(error) => RpcResponse::err(id, encoding_error("trigger/schema", error)),
        }),
        METHOD_TRIGGER_WATCH => match backend.watch().await {
            Ok(stream) => {
                let request_id = id.clone();
                let stdout_clone = stdout.clone();
                tokio::spawn(async move {
                    drive_trigger_stream(request_id, stream, stdout_clone).await;
                });
                Some(RpcResponse::ok(id, json!({ "watching": true })))
            }
            Err(error) => Some(RpcResponse::err(id, error.into())),
        },
        METHOD_TRIGGER_ACK => {
            let params = match deserialize_params::<TriggerAckParams>(request.params, false) {
                Ok(value) => value,
                Err(error) => {
                    write_response(&stdout, &RpcResponse::err(id, error)).await;
                    return;
                }
            };
            Some(match backend.ack(&params.event_id).await {
                Ok(()) => {
                    RpcResponse::ok(id, json!({ "event_id": params.event_id, "acked": true }))
                }
                Err(error) => RpcResponse::err(id, error.into()),
            })
        }
        METHOD_SLACK_CHAT_POST_MESSAGE => {
            let params = match deserialize_params::<ChatPostMessageParams>(request.params, false) {
                Ok(value) => value,
                Err(error) => {
                    write_response(&stdout, &RpcResponse::err(id, error)).await;
                    return;
                }
            };
            Some(match web.chat_post_message(params).await {
                Ok(value) => RpcResponse::ok(id, value),
                Err(error) => RpcResponse::err(id, slack_error_to_rpc(error)),
            })
        }
        METHOD_SLACK_CHAT_POST_EPHEMERAL => {
            let params = match deserialize_params::<ChatPostEphemeralParams>(request.params, false)
            {
                Ok(value) => value,
                Err(error) => {
                    write_response(&stdout, &RpcResponse::err(id, error)).await;
                    return;
                }
            };
            Some(match web.chat_post_ephemeral(params).await {
                Ok(value) => RpcResponse::ok(id, value),
                Err(error) => RpcResponse::err(id, slack_error_to_rpc(error)),
            })
        }
        METHOD_SLACK_SEND_DM => {
            let params = match deserialize_params::<SendDmParams>(request.params, false) {
                Ok(value) => value,
                Err(error) => {
                    write_response(&stdout, &RpcResponse::err(id, error)).await;
                    return;
                }
            };
            Some(match web.send_dm(params).await {
                Ok(value) => RpcResponse::ok(id, value),
                Err(error) => RpcResponse::err(id, slack_error_to_rpc(error)),
            })
        }
        other if other.starts_with("$/") => None,
        other => Some(method_not_found(id, &info.name, other)),
    };

    if let Some(response) = response {
        write_response(&stdout, &response).await;
    }
}

/// Map a [`SlackApiError`] to a JSON-RPC error payload that preserves Slack's
/// error code in `data.category` for programmatic handling.
///
/// Workflow phases get a stable, structured shape:
///
/// - `category = "invalid_request"` for caller-side validation failures.
/// - `category = "slack_error"` for `ok=false` responses; the original
///   Slack error code is in `data.slack_error` and the full body in
///   `data.body`.
/// - `category = "http"` for non-2xx responses, with `data.status`.
/// - `category = "transport"` / `"decode"` / `"missing_token"` for the rest.
fn slack_error_to_rpc(error: SlackApiError) -> RpcError {
    match error {
        SlackApiError::InvalidRequest(message) => RpcError {
            code: error_codes::INVALID_PARAMS,
            message,
            data: Some(json!({ "category": "invalid_request" })),
        },
        SlackApiError::MissingBotToken => RpcError {
            code: error_codes::INTERNAL_ERROR,
            message: "SLACK_BOT_TOKEN unset".into(),
            data: Some(json!({ "category": "missing_token" })),
        },
        SlackApiError::Slack { code, body } => RpcError {
            code: error_codes::INTERNAL_ERROR,
            message: format!("slack api error: {code}"),
            data: Some(json!({
                "category": "slack_error",
                "slack_error": code,
                "body": body,
            })),
        },
        SlackApiError::Http { status, body } => RpcError {
            code: error_codes::INTERNAL_ERROR,
            message: format!("slack http {status}"),
            data: Some(json!({
                "category": "http",
                "status": status,
                "body": body,
            })),
        },
        SlackApiError::Transport(message) => RpcError {
            code: error_codes::INTERNAL_ERROR,
            message: format!("slack transport: {message}"),
            data: Some(json!({ "category": "transport" })),
        },
        SlackApiError::Decode(message) => RpcError {
            code: error_codes::INTERNAL_ERROR,
            message: format!("slack decode: {message}"),
            data: Some(json!({ "category": "decode" })),
        },
    }
}

async fn drive_trigger_stream(
    request_id: Option<Value>,
    mut stream: animus_trigger_protocol::TriggerStream,
    stdout: Arc<Mutex<Stdout>>,
) {
    std::future::poll_fn(|cx: &mut Context<'_>| loop {
        match Pin::new(&mut stream).poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                let event_value = match serde_json::to_value(&event) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                let mut payload = serde_json::Map::new();
                if let Some(id) = request_id.clone() {
                    payload.insert("id".to_string(), id);
                }
                payload.insert("event".to_string(), event_value);
                let notification =
                    RpcNotification::new(NOTIFICATION_TRIGGER_EVENT, Some(Value::Object(payload)));
                let stdout = stdout.clone();
                tokio::spawn(async move {
                    write_notification(&stdout, &notification).await;
                });
            }
            Poll::Ready(Some(Err(error))) => {
                let rpc_error: RpcError = error.into();
                let error_value = match serde_json::to_value(&rpc_error) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                let mut payload = serde_json::Map::new();
                if let Some(id) = request_id.clone() {
                    payload.insert("id".to_string(), id);
                }
                payload.insert("error".to_string(), error_value);
                let notification =
                    RpcNotification::new(NOTIFICATION_TRIGGER_EVENT, Some(Value::Object(payload)));
                let stdout = stdout.clone();
                tokio::spawn(async move {
                    write_notification(&stdout, &notification).await;
                });
                return Poll::Ready(());
            }
            Poll::Ready(None) => return Poll::Ready(()),
            Poll::Pending => return Poll::Pending,
        }
    })
    .await;
}

async fn read_frame<R>(reader: &mut R) -> Result<Option<RpcRequest>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut buf = String::new();
    loop {
        buf.clear();
        let bytes = reader.read_line(&mut buf).await?;
        if bytes == 0 {
            return Ok(None);
        }
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<RpcRequest>(trimmed) {
            Ok(request) => return Ok(Some(request)),
            Err(_) => continue,
        }
    }
}

async fn write_response(stdout: &Arc<Mutex<Stdout>>, response: &RpcResponse) {
    write_frame(stdout, response).await;
}

async fn write_notification(stdout: &Arc<Mutex<Stdout>>, notification: &RpcNotification) {
    write_frame(stdout, notification).await;
}

async fn write_frame<T: serde::Serialize>(stdout: &Arc<Mutex<Stdout>>, frame: &T) {
    if let Ok(mut payload) = serde_json::to_string(frame) {
        payload.push('\n');
        let mut guard = stdout.lock().await;
        let _ = guard.write_all(payload.as_bytes()).await;
        let _ = guard.flush().await;
    }
}

fn initialize_response(
    id: Option<Value>,
    info: &PluginInfo,
    capabilities: &PluginCapabilities,
) -> RpcResponse {
    let result = InitializeResult {
        protocol_version: PROTOCOL_VERSION.to_string(),
        plugin_info: info.clone(),
        capabilities: capabilities.clone(),
    };
    match serde_json::to_value(result) {
        Ok(value) => RpcResponse::ok(id, value),
        Err(error) => RpcResponse::err(id, encoding_error("initialize", error)),
    }
}

fn health_response(
    id: Option<Value>,
    result: std::result::Result<animus_plugin_protocol::HealthCheckResult, TriggerBackendError>,
) -> RpcResponse {
    match result {
        Ok(health) => match serde_json::to_value(health) {
            Ok(value) => RpcResponse::ok(id, value),
            Err(error) => RpcResponse::err(id, encoding_error("health/check", error)),
        },
        Err(error) => RpcResponse::err(
            id,
            RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: format!("health/check failed: {error}"),
                data: Some(json!({ "status": HealthStatus::Unhealthy })),
            },
        ),
    }
}

fn deserialize_params<T: for<'de> Deserialize<'de>>(
    params: Option<Value>,
    allow_missing: bool,
) -> std::result::Result<T, RpcError> {
    match params {
        Some(value) => serde_json::from_value::<T>(value).map_err(|error| RpcError {
            code: error_codes::INVALID_PARAMS,
            message: format!("invalid params: {error}"),
            data: None,
        }),
        None => {
            if allow_missing {
                serde_json::from_value::<T>(Value::Object(serde_json::Map::new())).map_err(
                    |error| RpcError {
                        code: error_codes::INVALID_PARAMS,
                        message: format!("invalid params: {error}"),
                        data: None,
                    },
                )
            } else {
                Err(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "missing params".to_string(),
                    data: None,
                })
            }
        }
    }
}

/// Mirror the upstream runtime's interactive-stdin guard: if the binary is
/// launched directly from a terminal (no piped JSON-RPC, no `--manifest`),
/// print a hint to stderr and exit `2` instead of blocking forever on
/// `read_line`. Matches `animus_plugin_runtime::refuse_terminal_stdin`.
fn refuse_terminal_stdin(plugin_name: &str) {
    if std::io::stdin().is_terminal() {
        eprintln!("{plugin_name} is a STDIO plugin; pipe JSON-RPC on stdin or pass --manifest");
        std::process::exit(2);
    }
}

fn method_not_found(id: Option<Value>, plugin_name: &str, method: &str) -> RpcResponse {
    RpcResponse::err(
        id,
        RpcError {
            code: error_codes::METHOD_NOT_FOUND,
            message: format!("method '{method}' not implemented by {plugin_name}"),
            data: None,
        },
    )
}

fn encoding_error(method: &str, error: serde_json::Error) -> RpcError {
    RpcError {
        code: error_codes::INTERNAL_ERROR,
        message: format!("failed to encode {method} result: {error}"),
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SlackConfig;

    #[test]
    fn capabilities_include_outbound_methods() {
        let backend = SlackBackend::new(SlackConfig::for_testing("http://localhost"));
        let caps = compute_capabilities(&backend);
        assert!(caps
            .methods
            .iter()
            .any(|m| m == METHOD_SLACK_CHAT_POST_MESSAGE));
        assert!(caps
            .methods
            .iter()
            .any(|m| m == METHOD_SLACK_CHAT_POST_EPHEMERAL));
        assert!(caps.methods.iter().any(|m| m == METHOD_SLACK_SEND_DM));
        assert!(caps.methods.iter().any(|m| m == METHOD_TRIGGER_WATCH));
        assert!(caps.methods.iter().any(|m| m == METHOD_TRIGGER_SCHEMA));
        assert!(caps.methods.iter().any(|m| m == METHOD_TRIGGER_ACK));
    }

    #[test]
    fn slack_error_to_rpc_carries_slack_code_in_data() {
        let err = SlackApiError::Slack {
            code: "channel_not_found".into(),
            body: json!({"ok": false, "error": "channel_not_found"}),
        };
        let rpc = slack_error_to_rpc(err);
        assert_eq!(rpc.code, error_codes::INTERNAL_ERROR);
        let data = rpc.data.expect("data set");
        assert_eq!(data["category"], "slack_error");
        assert_eq!(data["slack_error"], "channel_not_found");
    }

    #[test]
    fn slack_error_to_rpc_maps_invalid_request_to_invalid_params() {
        let err = SlackApiError::InvalidRequest("channel must not be empty".into());
        let rpc = slack_error_to_rpc(err);
        assert_eq!(rpc.code, error_codes::INVALID_PARAMS);
        let data = rpc.data.expect("data set");
        assert_eq!(data["category"], "invalid_request");
    }

    #[test]
    fn slack_error_to_rpc_includes_http_status() {
        let err = SlackApiError::Http {
            status: 503,
            body: "down".into(),
        };
        let rpc = slack_error_to_rpc(err);
        let data = rpc.data.expect("data set");
        assert_eq!(data["status"], 503);
        assert_eq!(data["category"], "http");
    }
}
