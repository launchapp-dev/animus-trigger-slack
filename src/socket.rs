//! Slack Socket Mode WebSocket loop.
//!
//! Flow:
//!
//! 1. POST `https://slack.com/api/apps.connections.open` with the app-level
//!    token → receives a fresh `wss://...` URL.
//! 2. Open a WebSocket to that URL via `tokio-tungstenite`.
//! 3. Read JSON frames. Each Slack event arrives as an envelope:
//!    ```json
//!    {
//!      "envelope_id": "...",
//!      "type": "events_api",
//!      "payload": {
//!        "type": "event_callback",
//!        "event": { "type": "app_mention", ... }
//!      }
//!    }
//!    ```
//! 4. For every envelope: ack it (send `{"envelope_id": "..."}` back), then
//!    map the inner event with [`crate::events::map_slack_event`] and forward
//!    to the `mpsc::Sender`.
//! 5. On disconnect, re-POST `apps.connections.open` (Slack rotates URLs)
//!    with exponential backoff capped at 60s.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::config::SlackConfig;
use crate::events::map_slack_event;
use animus_trigger_protocol::TriggerEvent;

const RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
struct AppsConnectionsOpenResponse {
    ok: bool,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Top-level run loop, owned by `tokio::spawn` from `SlackBackend::watch`.
///
/// Loops forever (or until the receiver is dropped) reconnecting on socket
/// failure with exponential backoff.
pub async fn run_socket_loop(config: SlackConfig, tx: mpsc::Sender<TriggerEvent>) {
    let mut backoff = RECONNECT_INITIAL_BACKOFF;

    loop {
        if tx.is_closed() {
            debug!("trigger event channel closed; exiting socket loop");
            return;
        }

        match connect_and_run(&config, &tx).await {
            Ok(()) => {
                info!("slack socket loop exited cleanly; reconnecting");
                backoff = RECONNECT_INITIAL_BACKOFF;
            }
            Err(error) => {
                error!(?error, "slack socket loop errored; will reconnect");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, RECONNECT_MAX_BACKOFF);
            }
        }
    }
}

/// One connect → drain → disconnect cycle. Returning `Ok(())` means the
/// stream closed cleanly; returning `Err` triggers reconnection with backoff.
async fn connect_and_run(config: &SlackConfig, tx: &mpsc::Sender<TriggerEvent>) -> Result<()> {
    let wss_url = fetch_socket_url(config).await?;
    info!("opening slack socket mode connection");

    let (ws_stream, _) = connect_async(&wss_url)
        .await
        .with_context(|| format!("connect_async({wss_url})"))?;
    let (mut sink, mut stream) = ws_stream.split();

    while let Some(message) = stream.next().await {
        let message = message.context("websocket read")?;
        match message {
            Message::Text(text) => {
                handle_frame(&text, config, &mut sink, tx).await?;
            }
            Message::Ping(payload) => {
                sink.send(Message::Pong(payload))
                    .await
                    .context("send pong")?;
            }
            Message::Close(_) => {
                debug!("slack closed socket");
                return Ok(());
            }
            Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {
                // ignore
            }
        }
    }

    Ok(())
}

/// Handle one inbound WebSocket text frame.
async fn handle_frame<S>(
    text: &str,
    config: &SlackConfig,
    sink: &mut S,
    tx: &mpsc::Sender<TriggerEvent>,
) -> Result<()>
where
    S: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(error) => {
            warn!(?error, frame = %text, "failed to parse slack frame");
            return Ok(());
        }
    };

    let envelope_type = value.get("type").and_then(Value::as_str).unwrap_or("");

    match envelope_type {
        // First frame after socket open — no envelope_id, no payload.
        "hello" => {
            debug!("received slack hello envelope");
            return Ok(());
        }
        // Slack rotates servers; we should disconnect & reconnect.
        "disconnect" => {
            debug!("received slack disconnect envelope");
            return Err(anyhow!("slack requested disconnect"));
        }
        // Wire-level events Slack delivers to Socket Mode clients.
        "events_api" | "slash_commands" | "interactive" => {}
        other => {
            debug!(envelope_type = other, "ignoring slack envelope");
            return Ok(());
        }
    }

    // Ack first — Slack requires this within 3s of receipt.
    if let Some(envelope_id) = value.get("envelope_id").and_then(Value::as_str) {
        let ack = serde_json::to_string(&json!({ "envelope_id": envelope_id })).expect("ack json");
        if let Err(error) = sink.send(Message::Text(ack)).await {
            error!(?error, "failed to send envelope ack");
        }
    }

    if envelope_type != "events_api" {
        return Ok(());
    }

    let Some(event) = value.get("payload").and_then(|p| p.get("event")) else {
        return Ok(());
    };

    let channel = event.get("channel").and_then(Value::as_str).unwrap_or("");
    if !channel.is_empty() && !config.channel_allowed(channel) {
        debug!(channel, "skipping event for filtered channel");
        return Ok(());
    }

    let Some(trigger_event) = map_slack_event(event) else {
        return Ok(());
    };

    if tx.send(trigger_event).await.is_err() {
        return Err(anyhow!("trigger event channel closed"));
    }
    Ok(())
}

/// POST `apps.connections.open` to obtain a fresh `wss://...` URL.
async fn fetch_socket_url(config: &SlackConfig) -> Result<String> {
    let app_token = config
        .app_token
        .as_deref()
        .ok_or_else(|| anyhow!("SLACK_APP_TOKEN required"))?;
    let url = format!("{}/apps.connections.open", config.slack_api_base);
    let client = reqwest::Client::builder()
        .user_agent(concat!("animus-trigger-slack/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("reqwest client build")?;
    let response = client
        .post(&url)
        .bearer_auth(app_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "apps.connections.open returned HTTP {status}: {body}"
        ));
    }
    let parsed: AppsConnectionsOpenResponse = serde_json::from_str(&body)
        .with_context(|| format!("apps.connections.open body: {body}"))?;
    if !parsed.ok {
        return Err(anyhow!(
            "apps.connections.open ok=false: {}",
            parsed.error.unwrap_or_else(|| "unknown".into())
        ));
    }
    parsed
        .url
        .ok_or_else(|| anyhow!("apps.connections.open missing url"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetch_socket_url_returns_url_on_ok() {
        let mut server = mockito::Server::new_async().await;
        let body = json!({
            "ok": true,
            "url": "wss://wss-primary.slack.com/link/?token=abc"
        })
        .to_string();
        let mock = server
            .mock("POST", "/apps.connections.open")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let mut config = SlackConfig::for_testing(server.url());
        config.slack_api_base = server.url();
        let url = fetch_socket_url(&config).await.expect("should succeed");

        mock.assert_async().await;
        assert!(url.starts_with("wss://"));
    }

    #[tokio::test]
    async fn fetch_socket_url_errors_on_not_ok() {
        let mut server = mockito::Server::new_async().await;
        let body = json!({ "ok": false, "error": "invalid_auth" }).to_string();
        server
            .mock("POST", "/apps.connections.open")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let mut config = SlackConfig::for_testing(server.url());
        config.slack_api_base = server.url();
        let error = fetch_socket_url(&config).await.expect_err("should fail");
        assert!(
            error.to_string().contains("invalid_auth"),
            "expected invalid_auth, got {error}"
        );
    }

    #[tokio::test]
    async fn fetch_socket_url_errors_on_http_500() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/apps.connections.open")
            .with_status(500)
            .with_body("boom")
            .create_async()
            .await;

        let mut config = SlackConfig::for_testing(server.url());
        config.slack_api_base = server.url();
        let error = fetch_socket_url(&config).await.expect_err("should fail");
        assert!(
            error.to_string().contains("500"),
            "expected status 500 in error: {error}"
        );
    }
}
