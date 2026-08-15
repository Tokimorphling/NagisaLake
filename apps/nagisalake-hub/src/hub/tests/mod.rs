use super::*;
use axum::body::{Body, Bytes};
use nagisalake_comfyui::ComfyUiConfig;
use nagisalake_protocol::Register;
use nagisalake_worker::{
    HubConfig as WorkerHubConfig, HubTlsConfig as WorkerHubTlsConfig, StateConfig, Worker,
    WorkerConfig, WorkerIdentity,
};
use nagisalake_workflow::{InputBinding, WorkflowConfig};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

mod api;
mod config;
mod integration;
mod jobs;
mod sessions;

fn config() -> HubConfig {
    HubConfig {
        server:       ServerConfig::default(),
        auth:         AuthConfig {
            worker_token: Some("worker-secret".into()),
            consumer_token: Some("consumer-secret".into()),
            ..AuthConfig::default()
        },
        browser:      BrowserConfig::default(),
        database:     None,
        transport:    TransportConfig::default(),
        object_store: None,
        oauth:        None,
        rate_limit:   RateLimitConfig {
            enabled:             false,
            trust_forwarded_for: false,
        },
        log:          LogConfig::default(),
    }
}

/// Serves the router on an ephemeral port and returns its address.
async fn spawn_router() -> std::net::SocketAddr {
    let app = router(config()).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}
