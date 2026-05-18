use animus_plugin_protocol::{PluginInfo, PLUGIN_KIND_TRIGGER_BACKEND};
use animus_plugin_runtime::trigger_backend_main;
use animus_trigger_slack::backend::SlackBackend;
use animus_trigger_slack::config::SlackConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let config = SlackConfig::from_env()?;
    let backend = SlackBackend::new(config);

    let info = PluginInfo {
        name: env!("CARGO_PKG_NAME").into(),
        version: env!("CARGO_PKG_VERSION").into(),
        plugin_kind: PLUGIN_KIND_TRIGGER_BACKEND.into(),
        description: Some(env!("CARGO_PKG_DESCRIPTION").into()),
    };

    trigger_backend_main(info, backend).await
}
