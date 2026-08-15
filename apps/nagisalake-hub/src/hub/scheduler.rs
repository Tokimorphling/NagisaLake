use super::*;

/// How often the Hub scheduler scans the dispatch queue for claimable jobs.
pub(super) const SCHEDULER_INTERVAL: Duration = Duration::from_secs(2);

/// How long a claimed job stays leased before another scheduler pass can
/// reclaim it. Must be longer than the expected dispatch round-trip.
pub(super) const SCHEDULER_LEASE_SECONDS: i64 = 30;

/// The Hub backlog scheduler.
///
/// Continuously scans `job_dispatch_queue` for jobs that have been accepted
/// by quota but not yet bound to a Worker. For each claimable job it:
///
/// 1. Leases the job (prevents concurrent schedulers from double-dispatching).
/// 2. Finds an eligible online Worker (same workflow version, with capacity).
/// 3. Atomically reserves capacity on that Worker.
/// 4. Binds the job: `queued` → `received`, sets `worker_id`/`session_id`,
///    and creates a `dispatch_outbox` row for the existing outbox consumer.
/// 5. Removes the job from `job_dispatch_queue`.
///
/// If no eligible Worker is found the lease expires and the job stays queued.
pub(super) async fn run_scheduler(state: AppState) {
    let Some(store) = state.store.clone() else {
        return;
    };
    let mut ticker = tokio::time::interval(SCHEDULER_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let started = Instant::now();
        state
            .metrics
            .scheduler_passes_total
            .fetch_add(1, Ordering::Relaxed);
        if let Err(error) = schedule_pass(&state, &store).await {
            state
                .metrics
                .scheduler_errors_total
                .fetch_add(1, Ordering::Relaxed);
            warn!(?error, "scheduler pass failed");
        }
        state
            .metrics
            .scheduler_last_pass_duration_nanoseconds
            .store(
                started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                Ordering::Relaxed,
            );
    }
}

/// Claims and dispatches jobs from the global durable queue.
///
/// Queue ownership is the consumer organization, not necessarily the target
/// device organization, so limiting the scan to organizations with connected
/// workers would strand every cross-organization shared-device batch.
pub(super) async fn schedule_pass(state: &AppState, store: &PgStore) -> Result<(), HubError> {
    for _ in 0..32 {
        let now = now_unix_ms();
        let (organization_id, job_id) = match store
            .claim_dispatch_queue_job("hub-scheduler", SCHEDULER_LEASE_SECONDS, now)
            .await
        {
            Ok(Some(value)) => value,
            Ok(None) => break,
            Err(error) => {
                state
                    .metrics
                    .scheduler_errors_total
                    .fetch_add(1, Ordering::Relaxed);
                warn!(?error, "failed to claim dispatch queue job");
                break;
            }
        };
        state
            .metrics
            .scheduler_claimed_jobs_total
            .fetch_add(1, Ordering::Relaxed);
        if let Err(error) = dispatch_queued_job(state, store, &organization_id, &job_id, now).await
        {
            state
                .metrics
                .scheduler_errors_total
                .fetch_add(1, Ordering::Relaxed);
            warn!(?error, %job_id, %organization_id, "failed to dispatch queued job");
        }
    }
    Ok(())
}

/// Binds a queued job to an eligible Worker and hands it to the outbox consumer.
pub(super) async fn dispatch_queued_job(
    state: &AppState,
    store: &PgStore,
    organization_id: &str,
    job_id: &str,
    now: i64,
) -> Result<(), HubError> {
    // Load just enough of the job to find a matching worker.
    let Some((workflow_id, workflow_version, target_org, target_device, attempt)) =
        store.queued_job_info(organization_id, job_id).await?
    else {
        return Ok(());
    };

    // The admission path persisted an authorized exact device. Do not broaden
    // this to any Worker in its organization: grants are device-scoped.
    let attempt = u32::try_from(attempt)
        .map_err(|_| HubError::InvalidConfig("persisted dispatch attempt is invalid".into()))?;
    let command_id = dispatch_command_id(job_id, attempt);
    let selected = state
        .sessions
        .reserve_capacity(&command_id, |worker| {
            worker.organization_id == target_org
                && worker.worker_id == target_device
                && worker
                    .capabilities
                    .workflows
                    .iter()
                    .any(|wf| wf.id == workflow_id && wf.version == workflow_version)
        })
        .await;

    let Some(worker) = selected else {
        state
            .metrics
            .scheduler_unassigned_jobs_total
            .fetch_add(1, Ordering::Relaxed);
        // No eligible worker — lease will expire, job stays queued.
        return Ok(());
    };

    // Bind the job: queued → received, set worker fields, create outbox entry.
    let bound = match store
        .bind_queued_job(
            organization_id,
            job_id,
            &worker.organization_id,
            &worker.worker_id,
            &worker.session_id,
            now,
        )
        .await
    {
        Ok(bound) => bound,
        Err(error) => {
            state
                .sessions
                .release_capacity_reservation(
                    &worker.organization_id,
                    &worker.worker_id,
                    &worker.session_id,
                    &command_id,
                )
                .await;
            return Err(HubError::Store(error));
        }
    };
    if !bound {
        state
            .metrics
            .scheduler_unassigned_jobs_total
            .fetch_add(1, Ordering::Relaxed);
        state
            .sessions
            .release_capacity_reservation(
                &worker.organization_id,
                &worker.worker_id,
                &worker.session_id,
                &command_id,
            )
            .await;
        return Ok(());
    }

    state
        .metrics
        .scheduler_dispatched_jobs_total
        .fetch_add(1, Ordering::Relaxed);

    info!(
        %job_id,
        worker = %worker.worker_id,
        "scheduler dispatched queued job to worker"
    );
    Ok(())
}
