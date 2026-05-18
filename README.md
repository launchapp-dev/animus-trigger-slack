# animus-trigger-slack

Slack mentions + DMs trigger backend for [Animus](https://github.com/launchapp-dev/animus-cli).

## What it does

Connects to Slack via [Socket Mode](https://api.slack.com/apis/socket-mode) and emits Animus `TriggerEvent`s when:

- A user `@mentions` the bot in any channel the bot is a member of (`slack.mention`)
- A user sends the bot a direct message (`slack.dm`)

No public webhook URL needed — Socket Mode keeps a persistent WebSocket to Slack.
Both event types are emitted with `action_hint = "create_task"`. The daemon's
event router decides whether to actually queue a workflow / create a task.

The full Slack event JSON is preserved in `payload`, so workflow YAML can
template against `{{trigger.payload.text}}`, `{{trigger.payload.user}}`,
`{{trigger.payload.channel}}`, etc.

## Install

```bash
animus plugin install launchapp-dev/animus-trigger-slack
```

## Configure

1. Create a Slack app at https://api.slack.com/apps
2. Enable **Socket Mode** under "Settings → Socket Mode" and generate an
   app-level token with the `connections:write` scope. This token starts with
   `xapp-...`.
3. Under "OAuth & Permissions", add at minimum the following bot scopes:
   - `app_mentions:read` — receive `@mention` events
   - `im:history` — receive DM messages
   - `chat:write` — (optional) lets workflows reply
4. Install the app to your workspace; copy the bot token (`xoxb-...`).
5. Under "Event Subscriptions" enable events and subscribe the bot to:
   - `app_mention`
   - `message.im`

Then run Animus with both tokens in the environment:

```bash
export SLACK_APP_TOKEN=xapp-1-A...
export SLACK_BOT_TOKEN=xoxb-...
# Optional: restrict to a subset of channels
# export SLACK_FILTER_CHANNELS=C0123,C0456
animus daemon start
```

## Configuration reference

| Env var | Required | Description |
|---|---|---|
| `SLACK_APP_TOKEN` | yes | Socket Mode app-level token (`xapp-...`). |
| `SLACK_BOT_TOKEN` | yes | Bot user OAuth token (`xoxb-...`). |
| `SLACK_FILTER_CHANNELS` | no | Comma-separated channel IDs to allow. Default: all channels the bot can see. |

`--manifest` and `health/check` work without any tokens set, so plugin
discovery (`animus plugin list`) succeeds in shells without secrets. The
backend reports itself `Unhealthy` until both tokens are present.

## Event schema

| Slack event | TriggerEvent kind | `subject_id` | `payload` |
|---|---|---|---|
| `app_mention` | `slack.mention` | `None` | full Slack `event` JSON |
| `message` (with `channel_type == "im"`) | `slack.dm` | `None` | full Slack `event` JSON |

Event ids are built as `slack:<team>/<channel>/<ts>` so reconnect-after-restart
maps onto a stable id for daemon-level dedup.

## Reconnect behavior

The Socket Mode loop reconnects on any disconnect (Slack-initiated rotation,
network blip, etc.) with exponential backoff starting at 1s and capping at
60s. Each reconnect re-issues `apps.connections.open` because Slack rotates
the WSS URL.

## Design

- **Protocol:** [`animus-trigger-protocol`](https://github.com/launchapp-dev/animus-protocol) (trigger variant)
- **Naming:** repo, crate, and binary all named `animus-trigger-slack`
- **Core repo:** [Animus](https://github.com/launchapp-dev/animus-cli)

## License

MIT — see [LICENSE](LICENSE).
