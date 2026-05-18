//! Library surface for the `animus-trigger-slack` plugin.
//!
//! The binary entrypoint lives in `src/main.rs`. Modules below are public so
//! integration tests (and downstream embedders that want to wire the Slack
//! trigger without spawning a subprocess) can reach the `TriggerBackend`
//! implementation directly.

pub mod backend;
pub mod config;
pub mod events;
pub mod socket;
