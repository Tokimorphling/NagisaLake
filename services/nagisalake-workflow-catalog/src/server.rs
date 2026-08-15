//! Standalone workflow catalog server binary.

use nagisalake_hub_store::{PgStore, StoreConfig};
use nagisalake_rpc::{
    Layer, Principal, Server, ServerConfig, ServerContext, Service, TcpIncoming, layer_fn,
};
use nagisalake_workflow_catalog::{ListWorkflowsRequest, ListWorkflowsResponse, WorkflowCatalog};
use std::net::SocketAddr;
use tokio::signal;
use tracing::{info, warn};

#[derive(Debug, serde::Deserialize)]
struct Config {
    listen_addr:     SocketAddr,
    store:           StoreConfig,
    /// Trusted user id for the loopback caller (the Hub). Required: the
    /// catalog rejects every request as unauthenticated without it, which
    /// would break the Hub-to-catalog call path.
    trusted_user_id: String,
}

/// Service that attaches a fixed [`Principal`] to the context before
/// delegating to the inner service. Used by the catalog server to inject the
/// trusted loopback caller identity, so the inner service never has to read a
/// client-supplied `user_id`.
#[derive(Clone)]
struct TrustService<S> {
    inner:     S,
    principal: Principal,
}

impl<S> Service<ServerContext, ListWorkflowsRequest> for TrustService<S>
where
    S: Service<ServerContext, ListWorkflowsRequest> + Send + Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ListWorkflowsRequest,
    ) -> Result<Self::Response, Self::Error> {
        cx.set_principal(self.principal.clone());
        self.inner.call(cx, req).await
    }
}

/// Builds a [`Layer`] that wraps a service in [`TrustService`].
fn trust_loopback_principal(
    user_id: String,
) -> impl Layer<WorkflowCatalog, Service = TrustService<WorkflowCatalog>> {
    let principal = Principal::new(user_id, None);
    layer_fn(move |inner: WorkflowCatalog| TrustService {
        inner,
        principal: principal.clone(),
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nagisalake_workflow_catalog=info,nagisalake_rpc=debug".into()),
        )
        .init();

    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".into());
    let config_content = tokio::fs::read_to_string(&config_path).await?;
    let config: Config = toml::from_str(&config_content)?;

    // The catalog trusts the caller out of band. Refuse to start on a
    // non-loopback address: binding a LAN address without transport auth
    // (mTLS/HMAC) would let any reachable client read another tenant's
    // workflows.
    if !config.listen_addr.ip().is_loopback() {
        anyhow::bail!(
            "refusing to bind {}: workflow catalog has no transport auth; use a loopback address",
            config.listen_addr
        );
    }

    info!("Connecting to PostgreSQL");
    let store = PgStore::connect(&config.store).await?;
    info!("Running migrations");
    store.migrate().await?;

    let catalog = WorkflowCatalog::new(store);
    let server = Server::new(catalog)
        .layer(trust_loopback_principal(config.trusted_user_id.clone()))
        .config(ServerConfig {
            max_connections: 100,
            max_request_timeout: std::time::Duration::from_secs(30),
            ..ServerConfig::default()
        });

    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
    let incoming = TcpIncoming::new(listener);

    info!(
        "Workflow Catalog listening on {} (trusted caller: {})",
        config.listen_addr, config.trusted_user_id
    );

    tokio::select! {
        result = server.serve::<ListWorkflowsRequest, ListWorkflowsResponse, _>(incoming) => {
            if let Err(error) = result {
                warn!(?error, "Server error");
            }
        }
        _ = signal::ctrl_c() => {
            info!("Received SIGINT, shutting down");
        }
    }

    Ok(())
}
