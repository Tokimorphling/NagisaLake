use super::*;

/// Builds the Hub router without binding a listener.
pub async fn router(config: HubConfig) -> Result<Router, HubError> {
    router_with_state(config)
        .await
        .map(|(router, _state)| router)
}

/// Builds the router and hands back the shared state, so callers can start
/// background tasks that need it (see [`serve`]).
pub(super) async fn router_with_state(config: HubConfig) -> Result<(Router, AppState), HubError> {
    let state = AppState::new(config).await?;
    let router = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics_endpoint))
        .route("/v1/workers", get(list_workers))
        .route("/v1/workflows", get(list_workflows))
        .route("/v1/worker/connect", get(worker_connect))
        .route("/v1/artifacts/uploads", post(create_upload))
        .route(
            "/v1/artifacts/uploads/{artifact_id}/complete",
            post(complete_upload),
        )
        .route(
            "/v1/artifacts/{artifact_id}/download",
            get(download_artifact),
        )
        .route("/v1/jobs", post(submit_job))
        .route("/v1/jobs/{job_id}", get(get_job).delete(cancel_job))
        .merge(product_api::routes())
        // Serves the embedded console and SPA deep links. API prefixes keep
        // returning the JSON 404 envelope. Without the `embed-web` feature this
        // is only that JSON 404.
        .fallback(web_ui::fallback)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            observe_http_request,
        ))
        .with_state(state.clone());
    Ok((router, state))
}

/// Runs the Hub until Ctrl-C.
pub async fn serve(config: HubConfig) -> Result<(), HubError> {
    let listen = config.server.listen;
    let heartbeat_interval = Duration::from_secs(config.transport.heartbeat_interval_seconds);
    let (app, state) = router_with_state(config).await?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(HubError::Io)?;
    info!(
        %listen,
        console = web_ui::is_embedded(),
        heartbeat_timeout_seconds =
            heartbeat_interval.saturating_mul(HEARTBEAT_MISS_ALLOWANCE).as_secs(),
        "starting nagisalake Hub"
    );
    let session_reaper = tokio::spawn(reap_stale_sessions(
        state.sessions.clone(),
        heartbeat_interval,
    ));
    let revoked_credential_reaper = tokio::spawn(reap_revoked_credentials(state.sessions.clone()));
    let upload_reaper = tokio::spawn(reap_expired_uploads(state.clone()));
    let rate_limit_reaper = tokio::spawn(reap_idle_rate_limits(state.rate_limiter.clone()));
    let quota_reconciler = tokio::spawn(reconcile_quota_usage(state.clone()));
    let backlog_metrics = tokio::spawn(sample_backlog_metrics(state.clone()));
    let dispatch_consumer = tokio::spawn(consume_dispatch_outbox(state.clone()));
    let scheduler = tokio::spawn(run_scheduler(state.clone()));
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(HubError::Io);
    session_reaper.abort();
    revoked_credential_reaper.abort();
    upload_reaper.abort();
    rate_limit_reaper.abort();
    quota_reconciler.abort();
    backlog_metrics.abort();
    dispatch_consumer.abort();
    scheduler.abort();
    result
}

pub(super) async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(?error, "failed to install Ctrl-C handler");
    }
}
