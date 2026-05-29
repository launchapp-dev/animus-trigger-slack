//! Library surface for the `animus-trigger-slack` plugin.
//!
//! The binary entrypoint lives in `src/main.rs`. Modules below are public so
//! integration tests (and downstream embedders that want to wire the Slack
//! trigger without spawning a subprocess) can reach the `TriggerBackend`
//! implementation directly.
//!
//! As of v0.2.0 the plugin also exposes outbound RPC methods via
//! [`dispatch`] so workflow phases can post messages back to Slack.

pub mod backend;
pub mod config;
pub mod dispatch;
pub mod events;
pub mod socket;
pub mod web_api;
