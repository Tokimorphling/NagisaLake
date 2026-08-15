use super::*;

pub(super) const DISPATCH_CONSUMER_INTERVAL: Duration = Duration::from_secs(1);

/// Stable across the scheduler/outbox hand-off and retry/restart cycles so the
/// capacity reservation is settled by the ACK for the command that consumed it.
pub(super) fn dispatch_command_id(job_id: &str, attempt: u32) -> String {
    format!("dispatch:{job_id}:{attempt}")
}

#[derive(Debug)]
pub(super) struct CachedOutboxJobBinding {
    pub(super) organization_id:        String,
    pub(super) worker_organization_id: String,
    pub(super) worker_id:              String,
    pub(super) attempt:                u32,
    pub(super) session_id:             String,
}

/// Delivers durable outbox rows to workers connected to this Hub instance.
/// Multi-Hub deployments still need a shared session router, but a failed ACK
/// on a healthy local session no longer leaves a job pending forever.
pub(super) async fn consume_dispatch_outbox(state: AppState) {
    let Some(store) = state.store.clone() else {
        return;
    };
    let mut ticker = tokio::time::interval(DISPATCH_CONSUMER_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let started = Instant::now();
        state
            .metrics
            .dispatch_outbox_passes_total
            .fetch_add(1, Ordering::Relaxed);
        let entries = match store.claim_dispatches(now_unix_ms(), 32).await {
            Ok(entries) => entries,
            Err(error) => {
                state
                    .metrics
                    .dispatch_outbox_errors_total
                    .fetch_add(1, Ordering::Relaxed);
                warn!(?error, "failed to claim dispatch outbox rows");
                state
                    .metrics
                    .dispatch_outbox_last_pass_duration_nanoseconds
                    .store(
                        started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                        Ordering::Relaxed,
                    );
                continue;
            }
        };
        state
            .metrics
            .dispatch_outbox_claimed_total
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        for entry in entries {
            let counter = if dispatch_outbox_entry(&state, &store, entry).await {
                &state.metrics.dispatch_outbox_delivered_total
            } else {
                &state.metrics.dispatch_outbox_errors_total
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        state
            .metrics
            .dispatch_outbox_last_pass_duration_nanoseconds
            .store(
                started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                Ordering::Relaxed,
            );
    }
}

pub(super) async fn rebind_cached_outbox_job(
    state: &AppState,
    job_id: &str,
    session_id: &str,
    dispatch: &DispatchJob,
) -> Result<bool, HubError> {
    let binding = {
        let data = state.data.read().await;
        data.jobs.get(job_id).map(|job| CachedOutboxJobBinding {
            organization_id:        job.organization_id.clone(),
            worker_organization_id: job.worker_organization_id.clone(),
            worker_id:              job.view.worker_id.clone(),
            attempt:                job.dispatch.attempt,
            session_id:             job.view.session_id.clone(),
        })
    };
    let Some(binding) = binding else {
        return Ok(false);
    };
    if binding.attempt != dispatch.attempt {
        return Ok(false);
    }
    if state
        .sessions
        .current_session_id(&binding.worker_organization_id, &binding.worker_id)
        .await
        .as_deref()
        != Some(session_id)
    {
        return Ok(false);
    }
    let now = now_unix_ms();
    if let Some(store) = state.store.as_ref() {
        let rebound = store
            .rebind_job_session(
                &binding.organization_id,
                job_id,
                i64::from(binding.attempt),
                &binding.session_id,
                session_id,
                now,
            )
            .await?;
        if !rebound {
            if state
                .sessions
                .current_session_id(&binding.worker_organization_id, &binding.worker_id)
                .await
                .as_deref()
                != Some(session_id)
            {
                return Ok(false);
            }
            let Some(stored) = store.job(&binding.organization_id, job_id).await? else {
                return Ok(false);
            };
            let Some(stored_session_id) = stored.session_id.as_deref() else {
                return Ok(false);
            };
            if !store
                .rebind_job_session(
                    &binding.organization_id,
                    job_id,
                    i64::from(binding.attempt),
                    stored_session_id,
                    session_id,
                    now,
                )
                .await?
            {
                return Ok(false);
            }
        }
    }
    let Some(_session_guard) = state
        .sessions
        .guard_current_session(
            &binding.worker_organization_id,
            &binding.worker_id,
            session_id,
        )
        .await
    else {
        return Ok(false);
    };
    Ok(merge_cached_outbox_job(state, job_id, binding.attempt, session_id, dispatch, now).await)
}

pub(super) async fn merge_cached_outbox_job(
    state: &AppState,
    job_id: &str,
    expected_attempt: u32,
    session_id: &str,
    dispatch: &DispatchJob,
    now: i64,
) -> bool {
    let mut data = state.data.write().await;
    let Some(cached) = data.jobs.get_mut(job_id) else {
        return false;
    };
    if cached.view.state.is_terminal() || cached.dispatch.attempt != expected_attempt {
        return false;
    }
    session_id.clone_into(&mut cached.view.session_id);
    cached.view.updated_at_unix_ms = cached.view.updated_at_unix_ms.max(now);
    cached.dispatch = dispatch.clone();
    true
}

/// Materializes one bound, non-terminal database row into the scheduling
/// cache. This is primarily the hand-off between the durable batch scheduler
/// and the existing outbox consumer: a restart may hydrate an unbound `queued`
/// snapshot, which must be replaced by the committed binding before worker
/// events can be accepted.
pub(super) async fn cache_bound_outbox_job(
    state: &AppState,
    store: &PgStore,
    job: &nagisalake_hub_store::StoredJob,
    dispatch: &DispatchJob,
) -> Result<bool, HubError> {
    let Some(worker_organization_id) = job.worker_organization_id.as_deref() else {
        return Ok(false);
    };
    let Some(worker_id) = job.worker_id.as_deref() else {
        return Ok(false);
    };
    let Some(session_id) = job.session_id.as_deref() else {
        return Ok(false);
    };
    let state_name = parse_job_state(&job.state)?;
    if state_name == JobState::Queued || state_name.is_terminal() {
        return Ok(false);
    }
    let view = job_view_from_stored(
        job.clone(),
        &store.events_for_job(&job.organization_id, &job.id).await?,
    )?;
    let record = JobRecord {
        organization_id: job.organization_id.clone(),
        actor_id: job.actor_id.clone(),
        actor_kind: job.actor_kind.clone(),
        actor_user_id: job.actor_user_id.clone(),
        worker_organization_id: worker_organization_id.to_owned(),
        view,
        dispatch: dispatch.clone(),
        last_event: u64::try_from(job.last_event)
            .map_err(|_| HubError::InvalidConfig("persisted job sequence is invalid".into()))?,
    };
    if record.view.worker_id != worker_id || record.view.session_id != session_id {
        return Ok(false);
    }
    let mut data = state.data.write().await;
    match data.jobs.get(&job.id) {
        None => {
            data.jobs.insert(job.id.clone(), record);
            Ok(true)
        }
        Some(existing)
            if existing.dispatch.attempt == dispatch.attempt
                && existing.view.state == JobState::Queued =>
        {
            // A restart may have hydrated the queued child before the scheduler
            // bound it in PostgreSQL. Replace that unbound snapshot; merely
            // returning true would send the command while worker events still
            // see an empty binding and get rejected.
            data.jobs.insert(job.id.clone(), record);
            Ok(true)
        }
        Some(existing) => Ok(!existing.view.state.is_terminal()
            && existing.dispatch.attempt == dispatch.attempt
            && existing.organization_id == job.organization_id
            && existing.worker_organization_id == worker_organization_id
            && existing.view.worker_id == worker_id
            && existing.view.session_id == session_id),
    }
}

pub(super) async fn dispatch_outbox_entry(
    state: &AppState,
    store: &PgStore,
    entry: DispatchOutbox,
) -> bool {
    let job = match store.job(&entry.organization_id, &entry.job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => {
            return store
                .mark_dispatch_delivered(&entry.organization_id, &entry.job_id, entry.attempt)
                .await
                .is_ok();
        }
        Err(error) => {
            warn!(?error, job_id = %entry.job_id, "failed to read outbox job");
            return false;
        }
    };
    if job.state == "completed" || job.state == "failed" || job.state == "cancelled" {
        return store
            .mark_dispatch_delivered(&entry.organization_id, &entry.job_id, entry.attempt)
            .await
            .is_ok();
    }
    let (Some(worker_organization_id), Some(worker_id), Some(_bound_session_id)) = (
        job.worker_organization_id.as_deref(),
        job.worker_id.as_deref(),
        job.session_id.as_deref(),
    ) else {
        let _ = store
            .record_dispatch_error(
                &entry.organization_id,
                &entry.job_id,
                entry.attempt,
                "outbox job has no complete worker binding",
            )
            .await;
        return false;
    };
    // Ask the registry for the one session that matters. Listing the whole
    // organization cloned a WorkerView — capability labels included — for every
    // connected device, once per outbox row, once per second.
    let Some(session_id) = state
        .sessions
        .current_session_id(worker_organization_id, worker_id)
        .await
    else {
        let message = "no local worker session is available";
        let _ = store
            .record_dispatch_error(
                &entry.organization_id,
                &entry.job_id,
                entry.attempt,
                message,
            )
            .await;
        return false;
    };
    let parameters = match serde_json::from_str::<JsonValue>(&job.parameters_json) {
        Ok(value) => value,
        Err(error) => {
            let message = format!("invalid persisted job parameters: {error}");
            let _ = store
                .record_dispatch_error(
                    &entry.organization_id,
                    &entry.job_id,
                    entry.attempt,
                    &message,
                )
                .await;
            return false;
        }
    };
    let input_ids = match serde_json::from_str::<Vec<String>>(&job.input_artifact_ids_json) {
        Ok(value) => value,
        Err(error) => {
            let message = format!("invalid persisted job inputs: {error}");
            let _ = store
                .record_dispatch_error(
                    &entry.organization_id,
                    &entry.job_id,
                    entry.attempt,
                    &message,
                )
                .await;
            return false;
        }
    };
    let mut inputs = Vec::with_capacity(input_ids.len());
    for artifact_id in input_ids {
        let artifact = match store.artifact(&entry.organization_id, &artifact_id).await {
            Ok(Some(artifact)) if artifact.state == "ready" => artifact,
            Ok(_) => {
                let message = format!("input artifact {artifact_id} is not ready");
                let _ = store
                    .record_dispatch_error(
                        &entry.organization_id,
                        &entry.job_id,
                        entry.attempt,
                        &message,
                    )
                    .await;
                return false;
            }
            Err(error) => {
                warn!(?error, %artifact_id, job_id = %entry.job_id, "failed to read dispatch artifact");
                let _ = store
                    .record_dispatch_error(
                        &entry.organization_id,
                        &entry.job_id,
                        entry.attempt,
                        &error.to_string(),
                    )
                    .await;
                return false;
            }
        };
        let size_bytes = match u64::try_from(artifact.size_bytes) {
            Ok(value) => value,
            Err(_) => {
                let message = format!("input artifact {artifact_id} has an invalid size");
                let _ = store
                    .record_dispatch_error(
                        &entry.organization_id,
                        &entry.job_id,
                        entry.attempt,
                        &message,
                    )
                    .await;
                return false;
            }
        };
        let download = match state.objects.presign_get(&artifact.object_key).await {
            Ok(value) => value,
            Err(error) => {
                let message = error.to_string();
                let _ = store
                    .record_dispatch_error(
                        &entry.organization_id,
                        &entry.job_id,
                        entry.attempt,
                        &message,
                    )
                    .await;
                return false;
            }
        };
        inputs.push(JobInput {
            artifact_id,
            name: artifact.name,
            content_type: artifact.content_type,
            size_bytes,
            sha256: artifact.sha256,
            download,
        });
    }
    let attempt = match u32::try_from(entry.attempt) {
        Ok(value) => value,
        Err(_) => {
            let message = "persisted dispatch attempt is invalid";
            let _ = store
                .record_dispatch_error(
                    &entry.organization_id,
                    &entry.job_id,
                    entry.attempt,
                    message,
                )
                .await;
            return false;
        }
    };
    let dispatch = DispatchJob {
        command_id: dispatch_command_id(&job.id, attempt),
        job_id: job.id.clone(),
        attempt,
        workflow_id: job.workflow_id.clone(),
        workflow_version: job.workflow_version.clone(),
        parameters,
        inputs,
    };
    let command_id = dispatch.command_id.clone();
    match rebind_cached_outbox_job(state, &job.id, &session_id, &dispatch).await {
        Ok(true) => {}
        Ok(false) => match cache_bound_outbox_job(state, store, &job, &dispatch).await {
            Ok(true) => {}
            Ok(false) => {
                let message = "bound outbox job is not dispatchable";
                let _ = store
                    .record_dispatch_error(
                        &entry.organization_id,
                        &entry.job_id,
                        entry.attempt,
                        message,
                    )
                    .await;
                return false;
            }
            Err(error) => {
                warn!(?error, job_id = %job.id, "failed to cache bound outbox job");
                let _ = store
                    .record_dispatch_error(
                        &entry.organization_id,
                        &entry.job_id,
                        entry.attempt,
                        &error.to_string(),
                    )
                    .await;
                return false;
            }
        },
        Err(error) => {
            warn!(?error, job_id = %job.id, "failed to rebind outbox job");
            let _ = store
                .record_dispatch_error(
                    &entry.organization_id,
                    &entry.job_id,
                    entry.attempt,
                    &error.to_string(),
                )
                .await;
            return false;
        }
    }
    if !state
        .sessions
        .ensure_capacity_reservation(worker_organization_id, worker_id, &session_id, &command_id)
        .await
    {
        let message = "bound worker session has no dispatch capacity";
        let _ = store
            .record_dispatch_error(
                &entry.organization_id,
                &entry.job_id,
                entry.attempt,
                message,
            )
            .await;
        return false;
    }
    match state
        .sessions
        .send_command(
            worker_organization_id,
            worker_id,
            &session_id,
            &command_id,
            HubMessage::DispatchJob(dispatch),
            Duration::from_secs(state.config.transport.command_ack_timeout_seconds),
        )
        .await
    {
        Ok(ack) if ack.accepted => {
            match store
                .mark_dispatch_delivered(&entry.organization_id, &entry.job_id, entry.attempt)
                .await
            {
                Ok(()) => true,
                Err(error) => {
                    warn!(?error, job_id = %entry.job_id, "failed to mark dispatch delivered");
                    false
                }
            }
        }
        Ok(ack) => {
            mark_job_failed(state, &entry.job_id, ack.message).await;
            store
                .mark_dispatch_delivered(&entry.organization_id, &entry.job_id, entry.attempt)
                .await
                .is_ok()
        }
        Err(error) => {
            record_dispatch_error(state, &entry.job_id, error.to_string()).await;
            if let Err(store_error) = store
                .record_dispatch_error(
                    &entry.organization_id,
                    &entry.job_id,
                    entry.attempt,
                    &error.to_string(),
                )
                .await
            {
                warn!(?store_error, job_id = %entry.job_id, "failed to reschedule dispatch");
            }
            false
        }
    }
}
