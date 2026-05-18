//! Slack event JSON → `TriggerEvent` mapping.
//!
//! v0.1.0 surfaces two Slack event types:
//!
//! - `app_mention` → `slack.mention`
//! - `message` with `channel_type == "im"` → `slack.dm`
//!
//! Other Slack event payloads (channel messages, reactions, joins, ...) are
//! ignored. The full Slack event JSON is preserved in `payload` so workflow
//! YAML can template against `{{trigger.payload.text}}`, `{{trigger.payload.user}}`,
//! etc.

use animus_trigger_protocol::TriggerEvent;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

/// `kind` emitted for `app_mention` events.
pub const KIND_SLACK_MENTION: &str = "slack.mention";

/// `kind` emitted for `message.im` events (direct messages to the bot).
pub const KIND_SLACK_DM: &str = "slack.dm";

/// `action_hint` set on both mention and DM events. The daemon's event router
/// makes the final routing decision.
pub const ACTION_HINT_CREATE_TASK: &str = "create_task";

/// Map a raw Slack event payload (the inner `event` object from an
/// `event_callback` envelope) into an Animus [`TriggerEvent`], returning
/// `None` for event types this backend does not care about.
///
/// The caller is responsible for channel filtering — see
/// [`crate::config::SlackConfig::channel_allowed`].
pub fn map_slack_event(slack_event: &Value) -> Option<TriggerEvent> {
    let event_type = slack_event.get("type")?.as_str()?;

    match event_type {
        "app_mention" => Some(build_event(KIND_SLACK_MENTION, slack_event)),
        "message" => {
            let channel_type = slack_event
                .get("channel_type")
                .and_then(Value::as_str)
                .unwrap_or("");
            // Skip Slack's edited/deleted/threaded bot echo messages.
            let subtype = slack_event.get("subtype").and_then(Value::as_str);
            if channel_type == "im" && subtype.is_none() {
                Some(build_event(KIND_SLACK_DM, slack_event))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parse a Slack `ts` string (e.g. `"1715701234.000100"`) into a UTC
/// `DateTime`. Returns `None` on parse failure.
fn parse_slack_ts(ts: &str) -> Option<DateTime<Utc>> {
    let mut parts = ts.splitn(2, '.');
    let secs: i64 = parts.next()?.parse().ok()?;
    let frac_str = parts.next().unwrap_or("0");
    // Slack uses 6-digit microseconds. Pad/truncate to 9 for nanos.
    let nanos: u32 = {
        let mut padded = String::with_capacity(9);
        padded.push_str(frac_str);
        while padded.len() < 9 {
            padded.push('0');
        }
        padded.truncate(9);
        padded.parse().ok()?
    };
    Utc.timestamp_opt(secs, nanos).single()
}

/// Build a stable event id of the form
/// `slack:<team>/<channel>/<ts>` so reconnect-after-restart can
/// dedupe against the daemon's journal.
fn build_event_id(slack_event: &Value) -> String {
    let team = slack_event
        .get("team")
        .and_then(Value::as_str)
        .or_else(|| slack_event.get("team_id").and_then(Value::as_str))
        .unwrap_or("unknown");
    let channel = slack_event
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let ts = slack_event.get("ts").and_then(Value::as_str).unwrap_or("0");
    format!("slack:{team}/{channel}/{ts}")
}

fn build_event(kind: &str, slack_event: &Value) -> TriggerEvent {
    let occurred_at = slack_event
        .get("ts")
        .and_then(Value::as_str)
        .and_then(parse_slack_ts)
        .unwrap_or_else(Utc::now);

    TriggerEvent {
        id: build_event_id(slack_event),
        occurred_at,
        kind: kind.to_string(),
        payload: slack_event.clone(),
        subject_id: None,
        action_hint: Some(ACTION_HINT_CREATE_TASK.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_app_mention_to_slack_mention_event() {
        let event = json!({
            "type": "app_mention",
            "user": "U12345",
            "text": "<@U_BOT> hello",
            "ts": "1715701234.000100",
            "channel": "C9999",
            "team": "T123",
            "event_ts": "1715701234.000100"
        });
        let mapped = map_slack_event(&event).expect("should map");
        assert_eq!(mapped.kind, KIND_SLACK_MENTION);
        assert_eq!(mapped.id, "slack:T123/C9999/1715701234.000100");
        assert_eq!(mapped.action_hint.as_deref(), Some(ACTION_HINT_CREATE_TASK));
        assert_eq!(mapped.subject_id, None);
        assert_eq!(
            mapped.payload.get("user").and_then(Value::as_str),
            Some("U12345")
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
            "team": "T123"
        });
        let mapped = map_slack_event(&event).expect("should map");
        assert_eq!(mapped.kind, KIND_SLACK_DM);
        assert_eq!(mapped.id, "slack:T123/D1234/1715701300.000200");
        assert_eq!(mapped.action_hint.as_deref(), Some(ACTION_HINT_CREATE_TASK));
    }

    #[test]
    fn ignores_message_in_channel() {
        let event = json!({
            "type": "message",
            "channel_type": "channel",
            "user": "U12345",
            "text": "hi everyone",
            "ts": "1715701400.000300",
            "channel": "C9999",
            "team": "T123"
        });
        assert!(map_slack_event(&event).is_none());
    }

    #[test]
    fn ignores_message_with_subtype() {
        let event = json!({
            "type": "message",
            "channel_type": "im",
            "subtype": "message_changed",
            "ts": "1715701500.000400",
            "channel": "D1234",
            "team": "T123"
        });
        assert!(map_slack_event(&event).is_none());
    }

    #[test]
    fn ignores_unrelated_event_types() {
        let event = json!({
            "type": "reaction_added",
            "user": "U12345",
            "reaction": "thumbsup",
            "ts": "1715701600.000500"
        });
        assert!(map_slack_event(&event).is_none());
    }

    #[test]
    fn falls_back_to_now_when_ts_missing() {
        let event = json!({
            "type": "app_mention",
            "user": "U1",
            "text": "ping",
            "channel": "C1",
            "team": "T1"
        });
        let mapped = map_slack_event(&event).expect("should map");
        assert!(mapped.id.starts_with("slack:T1/C1/"));
        // occurred_at is set to "now" — just check it's recent.
        let now = Utc::now();
        let delta = (now - mapped.occurred_at).num_seconds().abs();
        assert!(delta < 5, "occurred_at not close to now: {delta}s away");
    }
}
