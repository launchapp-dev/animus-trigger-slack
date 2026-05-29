//! Slack Web API client for outbound messaging.
//!
//! Provides typed wrappers around the three Slack Web API calls exposed as
//! outbound RPC methods on this plugin:
//!
//! - [`SlackWebClient::chat_post_message`] → `chat.postMessage`
//! - [`SlackWebClient::chat_post_ephemeral`] → `chat.postEphemeral`
//! - [`SlackWebClient::conversations_open_im`] → `conversations.open` (single-user IM)
//!
//! `send_dm` is built on top of the last two — see
//! [`crate::dispatch`] for the composed dispatch path.
//!
//! The client is a thin layer over `reqwest`: it uses the configured
//! `slack_api_base` so tests can point at mockito, attaches the bot token as a
//! bearer credential, and unwraps Slack's `{ ok: bool, error?: string, ... }`
//! response envelope into a `Result<Value, SlackApiError>`. The full successful
//! response value is returned verbatim so workflow phases can read fields like
//! `ts` (for replying in a thread) or `channel.id` (returned by
//! `conversations.open`).

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Slack Web API failure mode.
#[derive(Debug, thiserror::Error)]
pub enum SlackApiError {
    /// Caller passed a malformed parameter (empty channel, etc.).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Bot token absent at config-load time.
    #[error("SLACK_BOT_TOKEN unset")]
    MissingBotToken,

    /// Slack returned `ok=false` with an `error` code (e.g. `channel_not_found`,
    /// `not_in_channel`, `invalid_auth`).
    #[error("slack api error: {code}")]
    Slack {
        /// Slack's `error` string from the response body.
        code: String,
        /// Full response body for diagnostic context.
        body: Value,
    },

    /// Slack returned a non-2xx HTTP status. Body is included for diagnostics.
    #[error("slack http {status}: {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body (or empty string on read failure).
        body: String,
    },

    /// Transport failure (DNS, timeout, TLS, ...).
    #[error("slack transport: {0}")]
    Transport(String),

    /// JSON decode failure on the response body.
    #[error("slack decode: {0}")]
    Decode(String),
}

/// Parameters for `slack/chat_post_message`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ChatPostMessageParams {
    /// Channel id (`Cxxxx`) or user id (`Uxxxx`) — Slack also accepts `#name`.
    pub channel: String,
    /// Plain text body. Optional only when `blocks` is set; both arms are
    /// validated together in [`SlackWebClient::chat_post_message`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Parent message timestamp for thread replies (`"1715701234.000100"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
    /// Block Kit JSON. Either a `Vec<Value>` literal or a pre-serialized
    /// string; we pass whatever JSON we got through, and Slack will reject
    /// malformed structures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Value>,
}

/// Parameters for `slack/chat_post_ephemeral`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ChatPostEphemeralParams {
    /// Channel id.
    pub channel: String,
    /// User id that should see the ephemeral message.
    pub user: String,
    /// Plain text body.
    pub text: String,
    /// Optional Block Kit JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Value>,
}

/// Parameters for `slack/send_dm`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SendDmParams {
    /// User id (`Uxxxx`). We resolve a private IM channel for them.
    pub user_id: String,
    /// Plain text body. Optional only when `blocks` is set; both arms are
    /// validated together in [`SlackWebClient::send_dm`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Optional Block Kit JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Value>,
}

/// A configured Slack Web API client.
///
/// Cheap to clone (the inner `reqwest::Client` is reference counted).
#[derive(Debug, Clone)]
pub struct SlackWebClient {
    http: Client,
    base_url: String,
    bot_token: Option<String>,
}

impl SlackWebClient {
    /// Build a new client with the given Web API base URL and bot token.
    ///
    /// Pass `None` for the bot token to defer authentication failures to
    /// call time — useful for keeping `--manifest` and `health/check`
    /// credential-free.
    pub fn new(base_url: impl Into<String>, bot_token: Option<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("animus-trigger-slack/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("reqwest client build")?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            bot_token: bot_token.filter(|s| !s.is_empty()),
        })
    }

    /// `chat.postMessage` — post a message to a channel (or thread).
    pub async fn chat_post_message(
        &self,
        params: ChatPostMessageParams,
    ) -> std::result::Result<Value, SlackApiError> {
        if params.channel.trim().is_empty() {
            return Err(SlackApiError::InvalidRequest(
                "channel must not be empty".into(),
            ));
        }
        if params.text.as_deref().map(str::is_empty).unwrap_or(true) && params.blocks.is_none() {
            return Err(SlackApiError::InvalidRequest(
                "either text or blocks must be provided".into(),
            ));
        }

        let mut body = serde_json::Map::new();
        body.insert("channel".into(), Value::String(params.channel));
        if let Some(text) = params.text {
            body.insert("text".into(), Value::String(text));
        }
        if let Some(thread_ts) = params.thread_ts {
            body.insert("thread_ts".into(), Value::String(thread_ts));
        }
        if let Some(blocks) = params.blocks {
            body.insert("blocks".into(), blocks);
        }
        self.post_json("chat.postMessage", Value::Object(body))
            .await
    }

    /// `chat.postEphemeral` — post a message visible only to one user.
    pub async fn chat_post_ephemeral(
        &self,
        params: ChatPostEphemeralParams,
    ) -> std::result::Result<Value, SlackApiError> {
        if params.channel.trim().is_empty() {
            return Err(SlackApiError::InvalidRequest(
                "channel must not be empty".into(),
            ));
        }
        if params.user.trim().is_empty() {
            return Err(SlackApiError::InvalidRequest(
                "user must not be empty".into(),
            ));
        }
        if params.text.is_empty() && params.blocks.is_none() {
            return Err(SlackApiError::InvalidRequest(
                "either text or blocks must be provided".into(),
            ));
        }

        let mut body = serde_json::Map::new();
        body.insert("channel".into(), Value::String(params.channel));
        body.insert("user".into(), Value::String(params.user));
        body.insert("text".into(), Value::String(params.text));
        if let Some(blocks) = params.blocks {
            body.insert("blocks".into(), blocks);
        }
        self.post_json("chat.postEphemeral", Value::Object(body))
            .await
    }

    /// `conversations.open` for a single user. Returns the full Slack response
    /// — callers usually read `result["channel"]["id"]`.
    pub async fn conversations_open_im(
        &self,
        user_id: &str,
    ) -> std::result::Result<Value, SlackApiError> {
        if user_id.trim().is_empty() {
            return Err(SlackApiError::InvalidRequest(
                "user_id must not be empty".into(),
            ));
        }
        let body = json!({ "users": user_id });
        self.post_json("conversations.open", body).await
    }

    /// `slack/send_dm` — opens an IM channel with the user and posts a message.
    /// Returns the `chat.postMessage` response value.
    pub async fn send_dm(&self, params: SendDmParams) -> std::result::Result<Value, SlackApiError> {
        let text = params.text.filter(|s| !s.is_empty());
        if text.is_none() && params.blocks.is_none() {
            return Err(SlackApiError::InvalidRequest(
                "either text or blocks must be provided".into(),
            ));
        }
        let open = self.conversations_open_im(&params.user_id).await?;
        let channel_id = open
            .get("channel")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SlackApiError::Decode("conversations.open response missing channel.id".into())
            })?
            .to_string();
        self.chat_post_message(ChatPostMessageParams {
            channel: channel_id,
            text,
            thread_ts: None,
            blocks: params.blocks,
        })
        .await
    }

    /// Shared POST helper. Returns the parsed JSON response on `ok=true`,
    /// otherwise maps to [`SlackApiError`].
    async fn post_json(
        &self,
        endpoint: &str,
        body: Value,
    ) -> std::result::Result<Value, SlackApiError> {
        let token = self
            .bot_token
            .as_deref()
            .ok_or(SlackApiError::MissingBotToken)?;
        let url = format!("{}/{}", self.base_url, endpoint);
        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await
            .map_err(|e| SlackApiError::Transport(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| SlackApiError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(SlackApiError::Http {
                status: status.as_u16(),
                body: text,
            });
        }
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| SlackApiError::Decode(format!("{e}: {text}")))?;
        let ok = parsed.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if ok {
            Ok(parsed)
        } else {
            let code = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            Err(SlackApiError::Slack { code, body: parsed })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    fn client_for(base: &str) -> SlackWebClient {
        SlackWebClient::new(base.to_string(), Some("xoxb-test".to_string())).expect("client build")
    }

    #[tokio::test]
    async fn chat_post_message_returns_ok_body_on_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat.postMessage")
            .match_header("authorization", "Bearer xoxb-test")
            .match_body(Matcher::PartialJson(json!({"channel": "C1", "text": "hi"})))
            .with_status(200)
            .with_body(json!({"ok": true, "ts": "1715701234.000100", "channel": "C1"}).to_string())
            .create_async()
            .await;
        let c = client_for(&server.url());
        let v = c
            .chat_post_message(ChatPostMessageParams {
                channel: "C1".into(),
                text: Some("hi".into()),
                thread_ts: None,
                blocks: None,
            })
            .await
            .expect("ok");
        mock.assert_async().await;
        assert_eq!(v["ts"], "1715701234.000100");
    }

    #[tokio::test]
    async fn chat_post_message_maps_slack_error() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/chat.postMessage")
            .with_status(200)
            .with_body(json!({"ok": false, "error": "channel_not_found"}).to_string())
            .create_async()
            .await;
        let c = client_for(&server.url());
        let err = c
            .chat_post_message(ChatPostMessageParams {
                channel: "Cmissing".into(),
                text: Some("hi".into()),
                thread_ts: None,
                blocks: None,
            })
            .await
            .expect_err("should fail");
        match err {
            SlackApiError::Slack { code, .. } => assert_eq!(code, "channel_not_found"),
            other => panic!("expected SlackApiError::Slack, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_post_message_requires_channel() {
        let c = client_for("http://example.invalid");
        let err = c
            .chat_post_message(ChatPostMessageParams {
                channel: "".into(),
                text: Some("hi".into()),
                ..Default::default()
            })
            .await
            .expect_err("empty channel rejected");
        assert!(matches!(err, SlackApiError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn chat_post_message_requires_text_or_blocks() {
        let c = client_for("http://example.invalid");
        let err = c
            .chat_post_message(ChatPostMessageParams {
                channel: "C1".into(),
                text: None,
                thread_ts: None,
                blocks: None,
            })
            .await
            .expect_err("missing body rejected");
        assert!(matches!(err, SlackApiError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn chat_post_message_propagates_thread_ts_and_blocks() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat.postMessage")
            .match_body(Matcher::PartialJson(json!({
                "channel": "C1",
                "thread_ts": "1715701234.000100",
                "blocks": [{"type": "section"}]
            })))
            .with_status(200)
            .with_body(json!({"ok": true, "ts": "1715701234.000200"}).to_string())
            .create_async()
            .await;
        let c = client_for(&server.url());
        let _ = c
            .chat_post_message(ChatPostMessageParams {
                channel: "C1".into(),
                text: Some("threaded".into()),
                thread_ts: Some("1715701234.000100".into()),
                blocks: Some(json!([{ "type": "section" }])),
            })
            .await
            .expect("ok");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn chat_post_ephemeral_requires_user() {
        let c = client_for("http://example.invalid");
        let err = c
            .chat_post_ephemeral(ChatPostEphemeralParams {
                channel: "C1".into(),
                user: "".into(),
                text: "hi".into(),
                blocks: None,
            })
            .await
            .expect_err("empty user rejected");
        assert!(matches!(err, SlackApiError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn chat_post_ephemeral_returns_message_ts_on_success() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/chat.postEphemeral")
            .match_body(Matcher::PartialJson(json!({
                "channel": "C1", "user": "U1", "text": "psst"
            })))
            .with_status(200)
            .with_body(json!({"ok": true, "message_ts": "1715701234.000100"}).to_string())
            .create_async()
            .await;
        let c = client_for(&server.url());
        let v = c
            .chat_post_ephemeral(ChatPostEphemeralParams {
                channel: "C1".into(),
                user: "U1".into(),
                text: "psst".into(),
                blocks: None,
            })
            .await
            .expect("ok");
        assert_eq!(v["message_ts"], "1715701234.000100");
    }

    #[tokio::test]
    async fn conversations_open_im_returns_channel_id() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/conversations.open")
            .match_body(Matcher::PartialJson(json!({"users": "U1"})))
            .with_status(200)
            .with_body(json!({"ok": true, "channel": {"id": "D123"}}).to_string())
            .create_async()
            .await;
        let c = client_for(&server.url());
        let v = c.conversations_open_im("U1").await.expect("ok");
        assert_eq!(v["channel"]["id"], "D123");
    }

    #[tokio::test]
    async fn send_dm_opens_im_then_posts() {
        let mut server = mockito::Server::new_async().await;
        let open = server
            .mock("POST", "/conversations.open")
            .with_status(200)
            .with_body(json!({"ok": true, "channel": {"id": "D123"}}).to_string())
            .create_async()
            .await;
        let post = server
            .mock("POST", "/chat.postMessage")
            .match_body(Matcher::PartialJson(json!({
                "channel": "D123", "text": "ping"
            })))
            .with_status(200)
            .with_body(json!({"ok": true, "ts": "1715701234.000100"}).to_string())
            .create_async()
            .await;
        let c = client_for(&server.url());
        let v = c
            .send_dm(SendDmParams {
                user_id: "U1".into(),
                text: Some("ping".into()),
                blocks: None,
            })
            .await
            .expect("ok");
        open.assert_async().await;
        post.assert_async().await;
        assert_eq!(v["ts"], "1715701234.000100");
    }

    #[tokio::test]
    async fn send_dm_accepts_blocks_only() {
        let mut server = mockito::Server::new_async().await;
        let open = server
            .mock("POST", "/conversations.open")
            .with_status(200)
            .with_body(json!({"ok": true, "channel": {"id": "D456"}}).to_string())
            .create_async()
            .await;
        let post = server
            .mock("POST", "/chat.postMessage")
            .match_body(Matcher::PartialJson(json!({
                "channel": "D456",
                "blocks": [{"type": "section"}]
            })))
            .with_status(200)
            .with_body(json!({"ok": true, "ts": "1715701234.000400"}).to_string())
            .create_async()
            .await;
        let c = client_for(&server.url());
        let v = c
            .send_dm(SendDmParams {
                user_id: "U2".into(),
                text: None,
                blocks: Some(json!([{ "type": "section" }])),
            })
            .await
            .expect("blocks-only DM should post");
        open.assert_async().await;
        post.assert_async().await;
        assert_eq!(v["ts"], "1715701234.000400");
    }

    #[tokio::test]
    async fn send_dm_requires_body() {
        let c = client_for("http://example.invalid");
        let err = c
            .send_dm(SendDmParams {
                user_id: "U1".into(),
                text: None,
                blocks: None,
            })
            .await
            .expect_err("missing body rejected");
        assert!(matches!(err, SlackApiError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn missing_bot_token_errors() {
        let mut server = mockito::Server::new_async().await;
        // No mock — the request never gets that far.
        let client = SlackWebClient::new(server.url(), None).expect("build");
        let err = client
            .chat_post_message(ChatPostMessageParams {
                channel: "C1".into(),
                text: Some("hi".into()),
                ..Default::default()
            })
            .await
            .expect_err("missing token rejected");
        assert!(matches!(err, SlackApiError::MissingBotToken));
        // Touch `server` so it stays alive long enough.
        let _ = &mut server;
    }

    #[tokio::test]
    async fn non_2xx_maps_to_http_error() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/chat.postMessage")
            .with_status(503)
            .with_body("temporarily unavailable")
            .create_async()
            .await;
        let c = client_for(&server.url());
        let err = c
            .chat_post_message(ChatPostMessageParams {
                channel: "C1".into(),
                text: Some("hi".into()),
                ..Default::default()
            })
            .await
            .expect_err("should fail");
        match err {
            SlackApiError::Http { status, .. } => assert_eq!(status, 503),
            other => panic!("expected Http error, got {other:?}"),
        }
    }
}
