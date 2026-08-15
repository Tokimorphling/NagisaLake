use anyhow::Context;
use clap::Parser;
use nagisalake_worker::{Worker, WorkerConfig};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Args {
    /// Path to the worker TOML configuration.
    #[arg(long, env = "NAGISALAKE_WORKER_CONFIG")]
    config: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // An unset RUST_LOG makes from_default_env() build an empty filter, which
    // discards every event instead of falling back to a level. That left the
    // worker silent wherever nobody exports RUST_LOG, containers especially.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let args = Args::parse();
    let config = WorkerConfig::load(&args.config)
        .with_context(|| format!("load worker config {}", args.config))?;
    let worker = Worker::from_config(config).await.context("build worker")?;
    let shutdown = CancellationToken::new();
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_shutdown.cancel();
        }
    });
    worker
        .run_until_cancelled(shutdown)
        .await
        .context("run worker")
}
