//! Outbound Slack Web API contract tests.
//!
//! Covers the public surface added in v0.2.0:
//!
//! - `slack/chat_post_message` (with thread_ts + Block Kit support)
//! - `slack/chat_post_ephemeral`
//! - `slack/send_dm` (composed `conversations.open` + `chat.postMessage`)
//!
//! All HTTP traffic is intercepted by `mockito`; no real Slack token or
//! network access is required.

use animus_trigger_slack::dispatch::{
    METHOD_SLACK_CHAT_POST_EPHEMERAL, METHOD_SLACK_CHAT_POST_MESSAGE, METHOD_SLACK_SEND_DM,
};
use animus_trigger_slack::web_api::{
    ChatPostEphemeralParams, ChatPostMessageParams, SendDmParams, SlackApiError, SlackWebClient,
};
use serde_json::json;

fn client_for(base: &str) -> SlackWebClient {
    SlackWebClient::new(base.to_string(), Some("xoxb-test".to_string())).expect("client build")
}

#[tokio::test]
async fn chat_post_message_round_trip() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/chat.postMessage")
        .match_header("authorization", "Bearer xoxb-test")
        .with_status(200)
        .with_body(json!({"ok": true, "ts": "1715701234.000100", "channel": "C1"}).to_string())
        .create_async()
        .await;
    let c = client_for(&server.url());
    let result = c
        .chat_post_message(ChatPostMessageParams {
            channel: "C1".into(),
            text: Some("ship it".into()),
            thread_ts: None,
            blocks: None,
        })
        .await
        .expect("post");
    mock.assert_async().await;
    assert_eq!(result["ts"], "1715701234.000100");
}

#[tokio::test]
async fn chat_post_message_in_thread() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/chat.postMessage")
        .match_body(mockito::Matcher::PartialJson(json!({
            "channel": "C1",
            "thread_ts": "1715701234.000100",
        })))
        .with_status(200)
        .with_body(json!({"ok": true, "ts": "1715701234.000200"}).to_string())
        .create_async()
        .await;
    let c = client_for(&server.url());
    let result = c
        .chat_post_message(ChatPostMessageParams {
            channel: "C1".into(),
            text: Some("re: shipping".into()),
            thread_ts: Some("1715701234.000100".into()),
            blocks: None,
        })
        .await
        .expect("post");
    mock.assert_async().await;
    assert_eq!(result["ts"], "1715701234.000200");
}

#[tokio::test]
async fn chat_post_ephemeral_round_trip() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/chat.postEphemeral")
        .match_body(mockito::Matcher::PartialJson(json!({
            "channel": "C1", "user": "U1", "text": "psst"
        })))
        .with_status(200)
        .with_body(json!({"ok": true, "message_ts": "1715701234.000100"}).to_string())
        .create_async()
        .await;
    let c = client_for(&server.url());
    let result = c
        .chat_post_ephemeral(ChatPostEphemeralParams {
            channel: "C1".into(),
            user: "U1".into(),
            text: "psst".into(),
            blocks: None,
        })
        .await
        .expect("post");
    mock.assert_async().await;
    assert_eq!(result["message_ts"], "1715701234.000100");
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
        .match_body(mockito::Matcher::PartialJson(json!({
            "channel": "D123", "text": "ready for review"
        })))
        .with_status(200)
        .with_body(json!({"ok": true, "ts": "1715701234.000300"}).to_string())
        .create_async()
        .await;
    let c = client_for(&server.url());
    let result = c
        .send_dm(SendDmParams {
            user_id: "U1".into(),
            text: Some("ready for review".into()),
            blocks: None,
        })
        .await
        .expect("send_dm");
    open.assert_async().await;
    post.assert_async().await;
    assert_eq!(result["ts"], "1715701234.000300");
}

#[tokio::test]
async fn slack_error_maps_to_slack_variant() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/chat.postMessage")
        .with_status(200)
        .with_body(json!({"ok": false, "error": "not_in_channel"}).to_string())
        .create_async()
        .await;
    let c = client_for(&server.url());
    let err = c
        .chat_post_message(ChatPostMessageParams {
            channel: "C-not-joined".into(),
            text: Some("ping".into()),
            thread_ts: None,
            blocks: None,
        })
        .await
        .expect_err("should fail");
    match err {
        SlackApiError::Slack { code, .. } => assert_eq!(code, "not_in_channel"),
        other => panic!("expected SlackApiError::Slack, got {other:?}"),
    }
}

#[tokio::test]
async fn http_error_maps_to_http_variant() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/chat.postMessage")
        .with_status(429)
        .with_body("rate limited")
        .create_async()
        .await;
    let c = client_for(&server.url());
    let err = c
        .chat_post_message(ChatPostMessageParams {
            channel: "C1".into(),
            text: Some("ping".into()),
            thread_ts: None,
            blocks: None,
        })
        .await
        .expect_err("should fail");
    match err {
        SlackApiError::Http { status, .. } => assert_eq!(status, 429),
        other => panic!("expected Http error, got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_request_caught_before_http() {
    // No server — request must short-circuit on param validation.
    let c = client_for("http://example.invalid");
    let err = c
        .chat_post_message(ChatPostMessageParams {
            channel: "".into(),
            text: Some("hi".into()),
            thread_ts: None,
            blocks: None,
        })
        .await
        .expect_err("empty channel rejected");
    assert!(matches!(err, SlackApiError::InvalidRequest(_)));
}

#[tokio::test]
async fn missing_bot_token_short_circuits() {
    let c = SlackWebClient::new("http://example.invalid".to_string(), None).expect("build");
    let err = c
        .chat_post_message(ChatPostMessageParams {
            channel: "C1".into(),
            text: Some("hi".into()),
            thread_ts: None,
            blocks: None,
        })
        .await
        .expect_err("missing token rejected");
    assert!(matches!(err, SlackApiError::MissingBotToken));
}

#[test]
fn outbound_method_constants_are_namespaced() {
    // Smoke test: the method names are stable enough for plugin.toml + README
    // to reference them by string. If anyone renames the constants, this trips.
    assert_eq!(METHOD_SLACK_CHAT_POST_MESSAGE, "slack/chat_post_message");
    assert_eq!(
        METHOD_SLACK_CHAT_POST_EPHEMERAL,
        "slack/chat_post_ephemeral"
    );
    assert_eq!(METHOD_SLACK_SEND_DM, "slack/send_dm");
}

#[test]
fn manifest_advertises_outbound_methods() {
    // Run the binary with --manifest and confirm the new outbound methods
    // appear in the capabilities list. This is the discovery surface the
    // plugin host reads when `animus plugin install` runs.
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_animus-trigger-slack"));
    let output = std::process::Command::new(&binary)
        .arg("--manifest")
        .env_remove("SLACK_APP_TOKEN")
        .env_remove("SLACK_BOT_TOKEN")
        .output()
        .expect("spawn binary");
    assert!(
        output.status.success(),
        "--manifest exited {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let manifest: serde_json::Value = serde_json::from_str(stdout.trim()).expect("manifest JSON");
    let caps = manifest["capabilities"]
        .as_array()
        .expect("capabilities array");
    let names: Vec<&str> = caps.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"slack/chat_post_message"));
    assert!(names.contains(&"slack/chat_post_ephemeral"));
    assert!(names.contains(&"slack/send_dm"));
    assert!(names.contains(&"trigger/watch"));
}
