//! [`SlackBackend`] - the `TriggerBackend` implementation for Slack.
//!
//! Holds a [`SlackConfig`] plus a `Mutex<Option<...>>` so `watch()` can hand
//! out the receiver stream exactly once. Spawns
//! [`crate::socket::run_socket_loop`] as a background task that owns the
//! Socket Mode WebSocket and pumps `TriggerEvent`s into the `mpsc::Sender`.

use animus_plugin_protocol::{HealthCheckResult, HealthStatus};
use animus_trigger_protocol::{BackendError, TriggerBackend, TriggerSchema, TriggerStream};
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::config::{SlackConfig, MISSING_APP_TOKEN_MSG, MISSING_BOT_TOKEN_MSG};
use crate::events::{KIND_SLACK_DM, KIND_SLACK_MENTION};
use crate::socket;

/// The Slack trigger backend.
pub struct SlackBackend {
    config: SlackConfig,
}

impl SlackBackend {
    /// Build a backend from configuration.
    pub fn new(config: SlackConfig) -> Self {
        Self { config }
    }

    /// Return the backend's configuration (read-only). Useful for tests.
    pub fn config(&self) -> &SlackConfig {
        &self.config
    }
}

#[async_trait]
impl TriggerBackend for SlackBackend {
    fn schema(&self) -> TriggerSchema {
        TriggerSchema {
            kinds: vec![KIND_SLACK_MENTION.to_string(), KIND_SLACK_DM.to_string()],
            // Socket Mode reconnects on its own across restarts, picking up
            // the next live envelope. We do not persist a cursor.
            supports_resume: true,
            // Slack delivers each envelope once per connection; if the daemon
            // restarts, Slack will redeliver missed events with the *same*
            // envelope id but our v0.1.0 event id is built from team/channel/ts
            // which is also stable, so dedup naturally works at the daemon
            // layer. Still, advertise `false` so the host maintains its own
            // dedup table.
            supports_dedup: false,
            // Slack acks happen in-socket per envelope; the `trigger/ack`
            // method is a no-op for v0.1.0 but we advertise it so the host
            // can call it without method-not-supported errors.
            supports_ack: true,
        }
    }

    async fn watch(&self) -> Result<TriggerStream, BackendError> {
        if self.config.app_token.as_deref().is_none_or(str::is_empty) {
            return Err(BackendError::Unavailable(MISSING_APP_TOKEN_MSG.to_string()));
        }
        if self.config.bot_token.as_deref().is_none_or(str::is_empty) {
            return Err(BackendError::Unavailable(MISSING_BOT_TOKEN_MSG.to_string()));
        }

        let (tx, rx) = mpsc::channel(256);
        let config = self.config.clone();
        tokio::spawn(async move {
            socket::run_socket_loop(config, tx).await;
        });
        let stream = ReceiverStream::new(rx).map(Ok);
        Ok(Box::pin(stream))
    }

    async fn ack(&self, _event_id: &str) -> Result<(), BackendError> {
        // Slack acks each envelope inside the socket loop, not per
        // Animus-event-id. v0.1.0 treats this as a no-op.
        Ok(())
    }

    async fn health(&self) -> Result<HealthCheckResult, BackendError> {
        let last_error = if self.config.app_token.as_deref().is_none_or(str::is_empty) {
            Some(MISSING_APP_TOKEN_MSG.to_string())
        } else if self.config.bot_token.as_deref().is_none_or(str::is_empty) {
            Some(MISSING_BOT_TOKEN_MSG.to_string())
        } else {
            None
        };
        let status = if last_error.is_some() {
            HealthStatus::Unhealthy
        } else {
            HealthStatus::Healthy
        };
        Ok(HealthCheckResult {
            status,
            uptime_ms: None,
            memory_usage_bytes: None,
            last_error,
        })
    }
}
