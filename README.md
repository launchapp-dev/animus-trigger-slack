# animus-trigger-slack

Slack mentions + DMs trigger backend for [Animus](https://github.com/launchapp-dev/animus-cli),
plus outbound Slack Web API methods workflow phases can call after human
review.

## What it does

**Inbound (since v0.1):** connects to Slack via
[Socket Mode](https://api.slack.com/apis/socket-mode) and emits Animus
`TriggerEvent`s when:

- A user `@mentions` the bot in any channel the bot is a member of (`slack.mention`)
- A user sends the bot a direct message (`slack.dm`)

No public webhook URL needed — Socket Mode keeps a persistent WebSocket to Slack.
Both event types are emitted with `action_hint = "create_task"`. The daemon's
event router decides whether to actually queue a workflow / create a task.

The full Slack event JSON is preserved in `payload`, so workflow YAML can
template against `{{trigger.payload.text}}`, `{{trigger.payload.user}}`,
`{{trigger.payload.channel}}`, etc.

**Outbound (new in v0.2):** workflow phases can post back to Slack via three
JSON-RPC methods exposed by the plugin — see
[Outbound RPC methods](#outbound-rpc-methods) below.

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

## Outbound RPC methods

These methods let a workflow phase send a Slack message after (for example)
a human-approval gate finishes. All three reuse `SLACK_BOT_TOKEN` and
require the bot to have the `chat:write` (and, for ephemeral, `chat:write.customize`)
scope. Errors are mapped to JSON-RPC error responses with a structured
`data.category` field so workflows can branch on failure mode
(`slack_error`, `http`, `transport`, `decode`, `invalid_request`,
`missing_token`).

### `slack/chat_post_message`

Wraps [`chat.postMessage`](https://api.slack.com/methods/chat.postMessage).

| Param | Type | Notes |
|---|---|---|
| `channel` | string, required | Channel id (`Cxxxx`), user id (`Uxxxx`), or `#name`. |
| `text` | string, optional | Plain-text body. Required unless `blocks` is set. |
| `thread_ts` | string, optional | Parent message timestamp to reply in a thread. |
| `blocks` | JSON, optional | [Block Kit](https://api.slack.com/block-kit) JSON. |

### `slack/chat_post_ephemeral`

Wraps [`chat.postEphemeral`](https://api.slack.com/methods/chat.postEphemeral).
The message is only visible to the target user.

| Param | Type | Notes |
|---|---|---|
| `channel` | string, required | Channel id. |
| `user` | string, required | User id who should see the message. |
| `text` | string, required | Plain-text body. |
| `blocks` | JSON, optional | Block Kit JSON. |

### `slack/send_dm`

Convenience method that calls
[`conversations.open`](https://api.slack.com/methods/conversations.open)
to obtain the bot↔user IM channel, then posts a message there. Returns the
`chat.postMessage` response.

| Param | Type | Notes |
|---|---|---|
| `user_id` | string, required | Target user id (`Uxxxx`). |
| `text` | string, optional | Plain-text body. Required unless `blocks` is set. |
| `blocks` | JSON, optional | Block Kit JSON. |

### Workflow YAML usage

```yaml
phases:
  - name: notify-author
    when: review.approved
    steps:
      - rpc:
          plugin: animus-trigger-slack
          method: slack/chat_post_message
          params:
            channel: "{{trigger.payload.channel}}"
            thread_ts: "{{trigger.payload.ts}}"
            text: ":white_check_mark: PR #{{task.pr_number}} approved and merged."

  - name: dm-reviewer
    when: review.requested
    steps:
      - rpc:
          plugin: animus-trigger-slack
          method: slack/send_dm
          params:
            user_id: "{{review.reviewer_slack_id}}"
            text: "You've been requested to review {{task.title}}."
```

## Design

- **Protocol:** [`animus-trigger-protocol`](https://github.com/launchapp-dev/animus-protocol) (trigger variant)
- **Naming:** repo, crate, and binary all named `animus-trigger-slack`
- **Core repo:** [Animus](https://github.com/launchapp-dev/animus-cli)
- **HTTP client:** the existing `reqwest` dependency — no extra Slack SDK
  is pulled in for the three outbound methods so the plugin stays small
  and the wire shape stays transparent.

## License

MIT — see [LICENSE](LICENSE).
