use animus_plugin_protocol::{PluginInfo, PLUGIN_KIND_TRIGGER_BACKEND};
use animus_trigger_slack::backend::SlackBackend;
use animus_trigger_slack::config::SlackConfig;
use animus_trigger_slack::dispatch::{
    METHOD_SLACK_CHAT_POST_EPHEMERAL, METHOD_SLACK_CHAT_POST_MESSAGE, METHOD_SLACK_SEND_DM,
};
use animus_trigger_slack::web_api::SlackWebClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    emit_manifest_if_requested();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let config = SlackConfig::from_env()?;
    let web = SlackWebClient::new(config.slack_api_base.clone(), config.bot_token.clone())?;
    let backend = SlackBackend::new(config);

    let info = PluginInfo {
        name: env!("CARGO_PKG_NAME").into(),
        version: env!("CARGO_PKG_VERSION").into(),
        plugin_kind: PLUGIN_KIND_TRIGGER_BACKEND.into(),
        description: Some(env!("CARGO_PKG_DESCRIPTION").into()),
    };

    animus_trigger_slack::dispatch::run(info, backend, web).await
}

fn emit_manifest_if_requested() {
    if !std::env::args()
        .skip(1)
        .any(|arg| arg == "--manifest" || arg == "-m")
    {
        return;
    }

    let manifest = serde_json::json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "plugin_kind": "trigger_backend",
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "protocol_version": animus_plugin_protocol::PROTOCOL_VERSION,
        "capabilities": [
            "trigger/watch",
            "trigger/schema",
            "trigger/ack",
            "health/check",
            METHOD_SLACK_CHAT_POST_MESSAGE,
            METHOD_SLACK_CHAT_POST_EPHEMERAL,
            METHOD_SLACK_SEND_DM,
        ],
        "env_required": [
            {
                "name": "SLACK_APP_TOKEN",
                "description": "Socket Mode app-level token.",
                "sensitive": true,
                "required": true
            },
            {
                "name": "SLACK_BOT_TOKEN",
                "description": "Bot user OAuth token.",
                "sensitive": true,
                "required": true
            },
            {
                "name": "SLACK_FILTER_CHANNELS",
                "description": "Comma-separated channel IDs to listen on.",
                "required": false
            },
            {
                "name": "SLACK_API_BASE",
                "description": "Override the Slack Web API base URL.",
                "required": false
            }
        ]
    });
    println!(
        "{}",
        serde_json::to_string(&manifest).expect("serialize manifest")
    );
    std::process::exit(0);
}
