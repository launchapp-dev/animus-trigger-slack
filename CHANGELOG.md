# Changelog

All notable changes to `animus-trigger-slack` will be documented here.

## v0.2.0

### Added

- **Outbound Slack RPC methods** callable from workflow phases:
  - `slack/chat_post_message` — wraps Slack's
    [`chat.postMessage`](https://api.slack.com/methods/chat.postMessage).
    Params: `channel`, `text` (or `blocks`), optional `thread_ts`, optional
    `blocks` (Block Kit JSON).
  - `slack/chat_post_ephemeral` — wraps
    [`chat.postEphemeral`](https://api.slack.com/methods/chat.postEphemeral).
    Params: `channel`, `user`, `text`, optional `blocks`.
  - `slack/send_dm` — opens a private IM channel with the user (via
    [`conversations.open`](https://api.slack.com/methods/conversations.open))
    then posts the message. Params: `user_id`, `text` (or `blocks`),
    optional `blocks`.
- New `SlackWebClient` (`src/web_api.rs`) with typed param structs and a
  `SlackApiError` enum that distinguishes invalid-request, missing-token,
  Slack API errors (`ok=false`), non-2xx HTTP, transport, and decode failures.
- New dispatch loop (`src/dispatch.rs`) that mirrors the upstream
  `trigger_backend_main` lifecycle + trigger surface and adds dispatch arms
  for the three outbound methods. Slack API errors map to JSON-RPC errors
  with a stable `data.category` discriminator (`slack_error`, `http`,
  `transport`, `decode`, `invalid_request`, `missing_token`) so workflow
  phases can react programmatically.
- `SLACK_API_BASE` documented in `plugin.toml` (was previously only honored
  by `SlackConfig::from_env`; now formal).

### Changed

- `plugin.toml` advertises the new methods in the `[capabilities].methods`
  list so plugin discovery surfaces them.
- `--manifest` output now lists the outbound methods so
  `animus plugin install` can pre-register them.

### Notes

- The Slack Web API client is built on the existing `reqwest` dependency —
  no new heavy crates (e.g. `slack-morphism`) were pulled in to keep the
  plugin small and the wire shape transparent.
- The bot token (`SLACK_BOT_TOKEN`) is reused for both the inbound Socket
  Mode connection and outbound Web API calls; no new env var is required.
- The `Mutex`-guarded shared `Stdout` ensures concurrent in-flight outbound
  calls don't interleave frames, matching the upstream runtime's locking
  model.

## v0.1.1

- Initial public release: Socket Mode inbound trigger for
  `app_mention` (→ `slack.mention`) and `message.im` (→ `slack.dm`) events.
