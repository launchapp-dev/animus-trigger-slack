//! Contract tests for `animus-trigger-slack`.
//!
//! The Socket Mode WebSocket loop is exercised by unit tests in
//! `src/socket.rs` (mockito-backed `apps.connections.open`). The tests here
//! cover the public `TriggerBackend` surface: schema, health behavior,
//! credential gating, and end-to-end `--manifest` invocation of the built
//! binary.

use animus_plugin_protocol::HealthStatus;
use animus_trigger_protocol::{BackendError, TriggerBackend};
use animus_trigger_slack::backend::SlackBackend;
use animus_trigger_slack::config::SlackConfig;
use animus_trigger_slack::events::{
    map_slack_event, ACTION_HINT_CREATE_TASK, KIND_SLACK_DM, KIND_SLACK_MENTION,
};
use serde_json::{json, Value};

fn empty_config() -> SlackConfig {
    SlackConfig::default()
}

#[tokio::test]
async fn schema_advertises_slack_kinds() {
    let backend = SlackBackend::new(SlackConfig::for_testing("http://localhost"));
    let schema = backend.schema();
    assert!(schema.kinds.iter().any(|k| k == KIND_SLACK_MENTION));
    assert!(schema.kinds.iter().any(|k| k == KIND_SLACK_DM));
    assert!(schema.supports_resume);
    assert!(!schema.supports_dedup);
    assert!(schema.supports_ack);
}

#[tokio::test]
async fn health_unhealthy_when_tokens_missing() {
    let backend = SlackBackend::new(empty_config());
    let health = backend.health().await.expect("health should not error");
    assert_eq!(health.status, HealthStatus::Unhealthy);
    let last_error = health.last_error.expect("last_error should be set");
    assert!(
        last_error.contains("SLACK_APP_TOKEN"),
        "expected SLACK_APP_TOKEN in last_error, got {last_error}"
    );
}

#[tokio::test]
async fn health_unhealthy_when_bot_token_missing() {
    let config = SlackConfig {
        app_token: Some("xapp-only".to_string()),
        ..SlackConfig::default()
    };
    let backend = SlackBackend::new(config);
    let health = backend.health().await.expect("health should not error");
    assert_eq!(health.status, HealthStatus::Unhealthy);
    let last_error = health.last_error.expect("last_error should be set");
    assert!(
        last_error.contains("SLACK_BOT_TOKEN"),
        "expected SLACK_BOT_TOKEN in last_error, got {last_error}"
    );
}

#[tokio::test]
async fn health_healthy_when_tokens_set() {
    let backend = SlackBackend::new(SlackConfig::for_testing("http://localhost"));
    let health = backend.health().await.expect("health should not error");
    assert_eq!(health.status, HealthStatus::Healthy);
    assert!(health.last_error.is_none());
}

#[tokio::test]
async fn watch_unavailable_when_credentials_missing() {
    let backend = SlackBackend::new(empty_config());
    match backend.watch().await {
        Ok(_) => panic!("watch should fail when credentials are missing"),
        Err(BackendError::Unavailable(message)) => assert!(
            message.contains("SLACK_APP_TOKEN"),
            "expected SLACK_APP_TOKEN in error: {message}"
        ),
        Err(other) => panic!("expected BackendError::Unavailable, got {other:?}"),
    }
}

#[test]
fn ack_is_no_op() {
    // Backend's ack is in-socket; the trigger/ack call should be a no-op.
    let backend = SlackBackend::new(SlackConfig::for_testing("http://localhost"));
    let result = futures::executor::block_on(backend.ack("doesnt-matter"));
    assert!(result.is_ok(), "ack should succeed");
}

#[test]
fn maps_app_mention_to_slack_mention_event() {
    let event = json!({
        "type": "app_mention",
        "user": "U12345",
        "text": "<@U_BOT> ping",
        "ts": "1715701234.000100",
        "channel": "C9999",
        "team": "T1"
    });
    let mapped = map_slack_event(&event).expect("should map");
    assert_eq!(mapped.kind, KIND_SLACK_MENTION);
    assert_eq!(mapped.action_hint.as_deref(), Some(ACTION_HINT_CREATE_TASK));
    assert_eq!(mapped.subject_id, None);
    assert_eq!(
        mapped.payload.get("text").and_then(Value::as_str),
        Some("<@U_BOT> ping")
    );
}

#[test]
fn maps_message_im_to_slack_dm_event() {
    let event = json!({
        "type": "message",
        "channel_type": "im",
        "user": "U12345",
        "text": "hi bot",
        "ts": "1715701300.000200",
        "channel": "D1234",
        "team": "T1"
    });
    let mapped = map_slack_event(&event).expect("should map");
    assert_eq!(mapped.kind, KIND_SLACK_DM);
    assert_eq!(mapped.action_hint.as_deref(), Some(ACTION_HINT_CREATE_TASK));
}

#[test]
fn ignores_unrelated_event_types() {
    let event = json!({
        "type": "message",
        "channel_type": "channel",
        "user": "U12345",
        "text": "group msg",
        "ts": "1715701400.000300",
        "channel": "C9999",
        "team": "T1"
    });
    assert!(map_slack_event(&event).is_none());

    let reaction = json!({
        "type": "reaction_added",
        "user": "U12345",
        "reaction": "wave",
        "ts": "1715701500.000400"
    });
    assert!(map_slack_event(&reaction).is_none());
}

#[test]
fn channel_filter_allows_all_when_empty() {
    let config = SlackConfig::for_testing("http://localhost");
    assert!(config.channel_allowed("C-anything"));
}

#[test]
fn channel_filter_restricts_when_set() {
    let mut config = SlackConfig::for_testing("http://localhost");
    config.filter_channels = vec!["C-allow".to_string()];
    assert!(config.channel_allowed("C-allow"));
    assert!(!config.channel_allowed("C-deny"));
}

#[test]
fn from_env_succeeds_without_credentials() {
    let _guard = EnvGuard::clear_all();
    let config = SlackConfig::from_env().expect("from_env must not require tokens");
    assert!(config.app_token.is_none());
    assert!(config.bot_token.is_none());
    assert!(config.filter_channels.is_empty());
}

/// Regression: the binary entrypoint must print manifest JSON without
/// requiring any environment variables. This invokes the real built
/// binary so we exercise `main()` end-to-end, mirroring how the plugin
/// host discovers plugins in `animus plugin list`.
#[test]
fn main_emits_manifest_without_credentials() {
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_animus-trigger-slack"));
    let output = std::process::Command::new(&binary)
        .arg("--manifest")
        .env_remove("SLACK_APP_TOKEN")
        .env_remove("SLACK_BOT_TOKEN")
        .env_remove("SLACK_FILTER_CHANNELS")
        .output()
        .expect("failed to spawn animus-trigger-slack");
    assert!(
        output.status.success(),
        "--manifest exited with {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
    let manifest: Value = serde_json::from_str(stdout.trim()).expect("stdout should be JSON");
    assert_eq!(manifest["name"], "animus-trigger-slack");
    assert_eq!(manifest["plugin_kind"], "trigger_backend");
}

/// Test helper: clear all Slack env vars and restore on drop.
struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn clear_all() -> Self {
        let keys = [
            "SLACK_APP_TOKEN",
            "SLACK_BOT_TOKEN",
            "SLACK_FILTER_CHANNELS",
        ];
        let mut previous = Vec::with_capacity(keys.len());
        for key in keys {
            previous.push((key, std::env::var(key).ok()));
            std::env::remove_var(key);
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
