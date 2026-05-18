//! Environment-driven configuration for the Slack trigger backend.
//!
//! `SlackConfig::from_env()` is intentionally lenient — both tokens are
//! optional at load time so that credential-free plugin lifecycle calls
//! (`--manifest`, `health`) succeed without secrets in the host shell.
//! Validation happens at the use site (`watch()`).

use anyhow::Result;

/// Last-error string surfaced by `health()` when the app token is missing.
pub const MISSING_APP_TOKEN_MSG: &str = "SLACK_APP_TOKEN unset";

/// Last-error string surfaced by `health()` when the bot token is missing.
pub const MISSING_BOT_TOKEN_MSG: &str = "SLACK_BOT_TOKEN unset";

/// Configuration for the Slack trigger backend.
#[derive(Debug, Clone, Default)]
pub struct SlackConfig {
    /// Socket Mode app-level token (`xapp-...`).
    pub app_token: Option<String>,
    /// Bot user OAuth token (`xoxb-...`).
    pub bot_token: Option<String>,
    /// Comma-separated channel ids to allow (empty = all channels).
    pub filter_channels: Vec<String>,
    /// Base URL for Slack web API (overridable for tests). Defaults to
    /// `https://slack.com/api`.
    pub slack_api_base: String,
}

impl SlackConfig {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let app_token = std::env::var("SLACK_APP_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        let bot_token = std::env::var("SLACK_BOT_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        let filter_channels = std::env::var("SLACK_FILTER_CHANNELS")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let slack_api_base = std::env::var("SLACK_API_BASE")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| "https://slack.com/api".to_string());

        Ok(Self {
            app_token,
            bot_token,
            filter_channels,
            slack_api_base,
        })
    }

    /// Test helper: build a fully-populated config pointed at a mock server.
    pub fn for_testing(slack_api_base: impl Into<String>) -> Self {
        Self {
            app_token: Some("xapp-test".to_string()),
            bot_token: Some("xoxb-test".to_string()),
            filter_channels: Vec::new(),
            slack_api_base: slack_api_base.into(),
        }
    }

    /// `true` if both tokens are present.
    pub fn has_credentials(&self) -> bool {
        self.app_token.as_deref().is_some_and(|s| !s.is_empty())
            && self.bot_token.as_deref().is_some_and(|s| !s.is_empty())
    }

    /// `true` if `channel` passes the channel allowlist. An empty allowlist
    /// allows every channel.
    pub fn channel_allowed(&self, channel: &str) -> bool {
        if self.filter_channels.is_empty() {
            return true;
        }
        self.filter_channels.iter().any(|c| c == channel)
    }
}
