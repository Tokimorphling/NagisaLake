use anyhow::Context;
use clap::Parser;
use nagisalake_hub::{HubConfig, serve};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Args {
    /// Path to the Hub TOML configuration.
    #[arg(long, env = "NAGISALAKE_HUB_CONFIG")]
    config: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = HubConfig::load(&args.config)
        .with_context(|| format!("load Hub config {}", args.config))?;
    // Config first so log.filter can feed the subscriber. Nothing above this
    // logs, and a load failure still surfaces through the process exit status.
    //
    // RUST_LOG wins when set. Without the try_ variant an unset RUST_LOG builds
    // an empty filter that discards every event, which is why a container with
    // no RUST_LOG produced no `docker logs` output at all.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&config.log.filter)),
        )
        .with_target(false)
        .init();
    serve(config).await.context("serve Hub")
}
