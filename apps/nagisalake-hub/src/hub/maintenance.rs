use super::*;

/// How often to sweep the in-memory revoked-credential set.
///
/// The set is append-only and every connection pays an O(N) `contains` check
/// against it, so leaving it unbounded both leaks memory and slows the hot
/// connect path. Entries whose sessions have already been evicted are safe to
/// drop: the database `revoked_at` column remains the durable check.
pub(super) const REVOKED_CREDENTIAL_REAP_INTERVAL: Duration = Duration::from_secs(60);

pub(super) async fn reap_revoked_credentials(sessions: SessionRegistry) {
    let mut ticker = tokio::time::interval(REVOKED_CREDENTIAL_REAP_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let removed = sessions.reap_revoked_credentials().await;
        if removed > 0 {
            debug!(removed, "reaped revoked credentials with no live session");
        }
    }
}

/// How many missed heartbeats to tolerate before a session is considered dead.
/// Three leaves room for one lost frame plus scheduling jitter.
pub(super) const HEARTBEAT_MISS_ALLOWANCE: u32 = 3;

/// Periodically drops worker sessions that stopped sending heartbeats.
///
/// A worker behind a network black hole or a half-open TCP connection never
/// closes its socket, so without this it stays listed as connected and keeps
/// receiving dispatches indefinitely.
pub(super) async fn reap_stale_sessions(sessions: SessionRegistry, heartbeat_interval: Duration) {
    let max_silence = heartbeat_interval.saturating_mul(HEARTBEAT_MISS_ALLOWANCE);
    let mut ticker = tokio::time::interval(heartbeat_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let reaped = sessions.reap_stale(max_silence).await;
        if !reaped.is_empty() {
            warn!(
                count = reaped.len(),
                workers = ?reaped,
                silence_seconds = max_silence.as_secs(),
                "reaped worker sessions with no recent heartbeat"
            );
        }
    }
}

/// How often to sweep expired pending uploads.
pub(super) const UPLOAD_REAP_INTERVAL: Duration = Duration::from_secs(60);
/// Cap per sweep so one pass cannot hold a long transaction or a burst of
/// delete calls against object storage.
pub(super) const UPLOAD_REAP_BATCH: i64 = 200;

/// Reclaims storage quota held by uploads that were reserved but never
/// completed.
///
/// Creating an upload reserves the full size before the presigned URL is
/// issued, so a client that never PUTs holds quota against zero stored bytes.
/// Quota and metadata are released together inside the store transaction; the
/// object delete happens afterwards because a leftover object is recoverable
/// while a wrong quota is not.
pub(super) async fn reap_expired_uploads(state: AppState) {
    let Some(store) = state.store.clone() else {
        return; // No control plane: nothing reserves quota.
    };
    let mut ticker = tokio::time::interval(UPLOAD_REAP_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        state
            .metrics
            .expired_upload_reaper_runs_total
            .fetch_add(1, Ordering::Relaxed);
        let reclaimed = match store
            .reclaim_expired_uploads(now_unix_ms(), UPLOAD_REAP_BATCH)
            .await
        {
            Ok(reclaimed) => reclaimed,
            Err(error) => {
                state
                    .metrics
                    .expired_upload_reaper_errors_total
                    .fetch_add(1, Ordering::Relaxed);
                warn!(?error, "failed to reclaim expired pending uploads");
                continue;
            }
        };
        state
            .metrics
            .expired_uploads_reclaimed_total
            .fetch_add(reclaimed.len() as u64, Ordering::Relaxed);
        if reclaimed.is_empty() {
            continue;
        }
        let released: i64 = reclaimed.iter().map(|upload| upload.size_bytes).sum();
        state
            .metrics
            .expired_upload_bytes_reclaimed_total
            .fetch_add(released.max(0) as u64, Ordering::Relaxed);
        for upload in &reclaimed {
            // The object usually does not exist, which is the whole point; a
            // failure here only leaves an orphan for the bucket lifecycle rule.
            if let Err(error) = state.objects.delete(&upload.object_key).await {
                state
                    .metrics
                    .expired_upload_delete_errors_total
                    .fetch_add(1, Ordering::Relaxed);
                warn!(
                    artifact_id = %upload.id,
                    ?error,
                    "could not delete object for an expired upload"
                );
            }
            state.data.write().await.artifacts.remove(&upload.id);
        }
        info!(
            count = reclaimed.len(),
            released_bytes = released,
            "reclaimed storage quota from expired pending uploads"
        );
    }
}

/// Keeps one-off address/account buckets from accumulating forever. The
/// limiter also has a hard cap, so this task is maintenance rather than the
/// only memory bound.
pub(super) async fn reap_idle_rate_limits(limiter: crate::ratelimit::RateLimiter) {
    let mut ticker = tokio::time::interval(Duration::from_secs(15 * 60));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let removed = limiter.evict_idle().await;
        if removed > 0 {
            debug!(removed, "evicted idle rate-limit buckets");
        }
    }
}

/// Queue gauges are sampled independently of `/metrics` so a Prometheus scrape
/// cannot contend with application requests for a database connection. Five
/// seconds is frequent enough for load diagnostics while keeping this
/// aggregate query off the one- and two-second scheduling hot loops.
pub(super) const BACKLOG_METRICS_INTERVAL: Duration = Duration::from_secs(5);
pub(super) const BACKLOG_METRICS_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) async fn sample_backlog_metrics(state: AppState) {
    let Some(store) = state.store.clone() else {
        return;
    };
    let mut ticker = tokio::time::interval(BACKLOG_METRICS_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let now = now_unix_ms();
        match tokio::time::timeout(BACKLOG_METRICS_TIMEOUT, store.backlog_metrics(now)).await {
            Ok(Ok(sample)) => {
                state
                    .metrics
                    .scheduler_queue_depth
                    .store(sample.dispatch_queue_depth, Ordering::Relaxed);
                state
                    .metrics
                    .scheduler_queue_oldest_ready_lag_milliseconds
                    .store(sample.dispatch_queue_oldest_ready_lag_ms, Ordering::Relaxed);
                state
                    .metrics
                    .dispatch_outbox_pending_depth
                    .store(sample.outbox_pending_depth, Ordering::Relaxed);
                state
                    .metrics
                    .dispatch_outbox_claimed_depth
                    .store(sample.outbox_claimed_depth, Ordering::Relaxed);
                state
                    .metrics
                    .dispatch_outbox_oldest_ready_lag_milliseconds
                    .store(sample.outbox_oldest_ready_lag_ms, Ordering::Relaxed);
                state
                    .metrics
                    .backlog_metrics_last_success_unix_seconds
                    .store(now.max(0) as u64 / 1_000, Ordering::Relaxed);
            }
            Ok(Err(error)) => {
                state
                    .metrics
                    .backlog_metrics_sample_errors_total
                    .fetch_add(1, Ordering::Relaxed);
                warn!(?error, "failed to sample scheduler/outbox backlog metrics");
            }
            Err(_) => {
                state
                    .metrics
                    .backlog_metrics_sample_errors_total
                    .fetch_add(1, Ordering::Relaxed);
                warn!(
                    timeout_seconds = BACKLOG_METRICS_TIMEOUT.as_secs(),
                    "scheduler/outbox backlog metrics query timed out"
                );
            }
        }
    }
}

pub(super) const QUOTA_RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub(super) const QUOTA_STALE_JOB_GRACE: Duration = Duration::from_secs(30 * 60);

pub(super) async fn reconcile_quota_usage(state: AppState) {
    let Some(store) = state.store.clone() else {
        return;
    };
    let mut ticker = tokio::time::interval(QUOTA_RECONCILE_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        state
            .metrics
            .quota_reconcile_runs_total
            .fetch_add(1, Ordering::Relaxed);
        match store
            .reconcile_active_job_quota(now_unix_ms(), QUOTA_STALE_JOB_GRACE.as_millis() as i64)
            .await
        {
            Ok(stats) if stats.corrected_organizations > 0 || stats.failed_jobs > 0 => {
                state
                    .metrics
                    .quota_reconcile_corrected_organizations_total
                    .fetch_add(stats.corrected_organizations, Ordering::Relaxed);
                state
                    .metrics
                    .quota_reconcile_failed_jobs_total
                    .fetch_add(stats.failed_jobs.max(0) as u64, Ordering::Relaxed);
                info!(
                    corrected_organizations = stats.corrected_organizations,
                    failed_jobs = stats.failed_jobs,
                    active_jobs = stats.active_jobs,
                    "reconciled organization job quota"
                );
            }
            Ok(_) => {}
            Err(error) => {
                state
                    .metrics
                    .quota_reconcile_errors_total
                    .fetch_add(1, Ordering::Relaxed);
                warn!(?error, "failed to reconcile organization job quota");
            }
        }
    }
}
