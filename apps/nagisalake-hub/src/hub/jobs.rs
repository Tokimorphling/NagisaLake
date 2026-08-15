use super::*;
use std::collections::BTreeSet;

/// Default and maximum page size for the job list.
pub(crate) const JOBS_PAGE_DEFAULT: i64 = 50;
pub(crate) const JOBS_PAGE_MAX: i64 = 200;

/// One page of the organization's jobs, newest first, without event timelines.
///
/// Reads the store rather than the cache: only non-terminal jobs stay resident,
/// so a cache-only list would hide a user's entire history. Bounded because the
/// unbounded version returned 120 MiB at 100k jobs while the page only renders
/// state and progress.
///
/// Returns the page plus the cursor for the next one, or `None` at the end.
pub(crate) async fn jobs_page_for_principal(
    state: &AppState,
    principal: &Principal,
    limit: Option<i64>,
    cursor: Option<&str>,
) -> Result<(Vec<JobSummary>, Option<String>), HubError> {
    let limit = limit.unwrap_or(JOBS_PAGE_DEFAULT).clamp(1, JOBS_PAGE_MAX);
    let Some(store) = state.store.as_ref() else {
        // Legacy mode without a control plane: the cache is the only copy.
        let mut jobs = jobs_from_cache(state, principal).await;
        jobs.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        return Ok((jobs, None));
    };
    let after = match cursor {
        Some(cursor) => Some(decode_job_cursor(cursor)?),
        None => None,
    };
    // Fetch one extra row to learn whether another page exists without a count.
    let mut rows = store
        .jobs_page(
            &principal.organization_id,
            limit + 1,
            after.as_ref().map(|(at, id)| (*at, id.as_str())),
        )
        .await
        .map_err(HubError::Store)?;
    let has_more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    if has_more {
        rows.pop();
    }
    let next_cursor = has_more
        .then(|| rows.last())
        .flatten()
        .map(|last| encode_job_cursor(last.created_at, &last.id));
    let jobs = rows.into_iter().map(JobSummary::from).collect();
    Ok((jobs, next_cursor))
}

/// Cache-only listing, used when no control plane is configured.
pub(super) async fn jobs_from_cache(state: &AppState, principal: &Principal) -> Vec<JobSummary> {
    let mut jobs = state
        .data
        .read()
        .await
        .jobs
        .values()
        .filter(|job| job.organization_id == principal.organization_id)
        .map(|job| JobSummary::from(&job.view))
        .collect::<Vec<_>>();
    jobs.sort_by(|left, right| {
        right
            .created_at_unix_ms
            .cmp(&left.created_at_unix_ms)
            .then_with(|| right.id.cmp(&left.id))
    });
    jobs
}

/// Opaque `created_at:id` cursor. Opaque so the shape can change later without
/// breaking clients that only echo it back.
pub(super) fn encode_job_cursor(created_at: i64, id: &str) -> String {
    data_encoding::BASE64URL_NOPAD.encode(format!("{created_at}:{id}").as_bytes())
}

pub(super) fn decode_job_cursor(cursor: &str) -> Result<(i64, String), HubError> {
    let invalid = || HubError::InvalidRequest("cursor is not valid".into());
    let raw = data_encoding::BASE64URL_NOPAD
        .decode(cursor.as_bytes())
        .map_err(|_| invalid())?;
    let decoded = String::from_utf8(raw).map_err(|_| invalid())?;
    let (created_at, id) = decoded.split_once(':').ok_or_else(invalid)?;
    Ok((created_at.parse().map_err(|_| invalid())?, id.to_owned()))
}

impl From<nagisalake_hub_store::StoredJob> for JobSummary {
    fn from(job: nagisalake_hub_store::StoredJob) -> Self {
        // Malformed persisted JSON degrades to an empty value rather than
        // failing the whole page; the detail view surfaces the real error.
        Self {
            id:                  job.id,
            workflow_id:         job.workflow_id,
            workflow_version:    job.workflow_version,
            parameters:          serde_json::from_str(&job.parameters_json)
                .unwrap_or(JsonValue::Null),
            input_artifact_ids:  serde_json::from_str(&job.input_artifact_ids_json)
                .unwrap_or_default(),
            output_artifact_ids: serde_json::from_str(&job.output_artifact_ids_json)
                .unwrap_or_default(),
            worker_id:           job.worker_id.unwrap_or_default(),
            session_id:          job.session_id.unwrap_or_default(),
            state:               parse_job_state(&job.state).unwrap_or(JobState::Queued),
            progress:            job.progress,
            prompt_id:           job.prompt_id,
            error:               job.error,
            created_at_unix_ms:  job.created_at,
            updated_at_unix_ms:  job.updated_at,
        }
    }
}

/// Loads one job, falling back to the store for jobs that are no longer
/// resident.
///
/// Only non-terminal jobs stay in memory, so a completed job is served from
/// PostgreSQL. It is intentionally not inserted into the cache: doing that would
/// grow it back to a full mirror one detail view at a time.
pub(crate) async fn job_for_principal(
    state: &AppState,
    principal: &Principal,
    job_id: &str,
) -> Result<JobView, HubError> {
    let cached = state
        .data
        .read()
        .await
        .jobs
        .get(job_id)
        .filter(|job| job.organization_id == principal.organization_id)
        .map(|job| job.view.clone());
    if let Some(view) = cached {
        return Ok(view);
    }
    if let Some(view) = state.cached_job(&principal.organization_id, job_id).await {
        return Ok(view);
    }
    if let Some(store) = state.store.as_ref() {
        let stored = store
            .job(&principal.organization_id, job_id)
            .await
            .map_err(HubError::Store)?;
        if let Some(job) = stored {
            let events = store
                .events_for_job(&principal.organization_id, job_id)
                .await
                .map_err(HubError::Store)?;
            let view = job_view_from_stored(job, &events)?;
            if view.state.is_terminal() {
                state
                    .cache_job(&principal.organization_id, job_id, view.clone())
                    .await;
            }
            return Ok(view);
        }
    }
    Err(HubError::NotFound("job".into()))
}

/// Rebuilds a [`JobView`] from persisted rows.
pub(super) fn job_view_from_stored(
    job: nagisalake_hub_store::StoredJob,
    events: &[nagisalake_hub_store::StoredJobEvent],
) -> Result<JobView, HubError> {
    let input_artifact_ids: Vec<String> = serde_json::from_str(&job.input_artifact_ids_json)
        .map_err(|error| {
            HubError::InvalidConfig(format!("invalid persisted job inputs: {error}"))
        })?;
    let output_artifact_ids: Vec<String> = serde_json::from_str(&job.output_artifact_ids_json)
        .map_err(|error| {
            HubError::InvalidConfig(format!("invalid persisted job outputs: {error}"))
        })?;
    let parameters: JsonValue = serde_json::from_str(&job.parameters_json).map_err(|error| {
        HubError::InvalidConfig(format!("invalid persisted job parameters: {error}"))
    })?;
    let events = events
        .iter()
        .filter(|event| event.job_id == job.id)
        .map(|event| {
            Ok(JobEventView {
                sequence: u64::try_from(event.sequence).map_err(|_| {
                    HubError::InvalidConfig("persisted event sequence is invalid".into())
                })?,
                kind:     parse_event_kind(&event.kind)?,
                progress: event.progress,
                message:  event.message.clone(),
                unix_ms:  event.unix_ms,
            })
        })
        .collect::<Result<Vec<_>, HubError>>()?;
    Ok(JobView {
        id: job.id,
        workflow_id: job.workflow_id,
        workflow_version: job.workflow_version,
        parameters,
        input_artifact_ids,
        output_artifact_ids,
        worker_id: job.worker_id.unwrap_or_default(),
        session_id: job.session_id.unwrap_or_default(),
        state: parse_job_state(&job.state)?,
        progress: job.progress,
        prompt_id: job.prompt_id,
        error: job.error,
        events,
        created_at_unix_ms: job.created_at,
        updated_at_unix_ms: job.updated_at,
    })
}

pub(crate) async fn cancel_job_for_principal(
    state: &AppState,
    principal: &Principal,
    job_id: &str,
) -> Result<JobView, HubError> {
    let cached = state
        .data
        .read()
        .await
        .jobs
        .get(job_id)
        .filter(|job| job.organization_id == principal.organization_id)
        .cloned();
    let Some(record) = cached else {
        // Only non-terminal jobs are resident, so a miss means the job is either
        // already finished or does not exist. Distinguish the two so callers get
        // "cannot be cancelled" instead of a misleading 404.
        if let Some(store) = state.store.as_ref()
            && let Some(stored) = store
                .job(&principal.organization_id, job_id)
                .await
                .map_err(HubError::Store)?
        {
            if stored.state == "queued" {
                let actor_user_id = stored.actor_user_id.as_deref();
                if !(principal.allows(Permission::JobsCancelAny)
                    || principal.allows(Permission::JobsCancelOwn)
                        && principal.user_id.as_deref() == actor_user_id)
                {
                    return Err(HubError::Forbidden(
                        "role can only cancel jobs created by the same user".into(),
                    ));
                }
                let now = now_unix_ms();
                if store
                    .cancel_queued_job(&principal.organization_id, job_id, now)
                    .await?
                {
                    let events = store
                        .events_for_job(&principal.organization_id, job_id)
                        .await?;
                    let stored = store
                        .job(&principal.organization_id, job_id)
                        .await?
                        .ok_or_else(|| HubError::NotFound("job".into()))?;
                    return job_view_from_stored(stored, &events);
                }
                return Err(HubError::Conflict(
                    "job advanced while cancellation was being committed".into(),
                ));
            }
            return Err(HubError::Conflict(
                "job cannot be cancelled after output upload begins".into(),
            ));
        }
        return Err(HubError::NotFound("job".into()));
    };
    if !principal_can_cancel_job(principal, &record) {
        return Err(HubError::Forbidden(
            "role can only cancel jobs created by the same user".into(),
        ));
    }
    if record.view.state.is_terminal() || record.view.state == JobState::Uploading {
        return Err(HubError::Conflict(
            "job cannot be cancelled after output upload begins".into(),
        ));
    }
    send_job_cancellation(state, &record).await
}

/// Cancels a job on whichever session the worker holds right now, falling back
/// to a local cancellation when the worker is not connected at all.
///
/// Addressing `record.view.session_id` is what made stalled jobs uncancellable.
/// After a reconnect that id names a socket nobody reads: if the registry has
/// already replaced it the send fails as "not connected" even though the worker
/// is right there, and if the dead session is still awaiting its reaper the send
/// blocks for the full ACK timeout and then fails anyway.
pub(super) async fn send_job_cancellation(
    state: &AppState,
    record: &JobRecord,
) -> Result<JobView, HubError> {
    if record.view.state == JobState::Queued {
        return cancel_job_locally(state, record).await;
    }
    let session_id = state
        .sessions
        .current_session_id(&record.worker_organization_id, &record.view.worker_id)
        .await;
    let Some(session_id) = session_id else {
        // Nothing is going to move this job: the worker is gone, and when it
        // returns it resumes from its own journal. Cancel here so the job stops
        // holding quota, and let the worker's own events reconcile later.
        return cancel_job_locally(state, record).await;
    };
    let command_id = Uuid::new_v4().to_string();
    let command = CancelJob {
        command_id: command_id.clone(),
        job_id:     record.view.id.clone(),
        reason:     "consumer requested cancellation".into(),
    };
    match state
        .sessions
        .send_command(
            &record.worker_organization_id,
            &record.view.worker_id,
            &session_id,
            &command_id,
            HubMessage::CancelJob(command),
            Duration::from_secs(state.config.transport.command_ack_timeout_seconds),
        )
        .await
    {
        Ok(ack) if ack.accepted => Ok(record.view.clone()),
        Ok(ack) => Err(HubError::Conflict(if ack.message.is_empty() {
            "worker rejected cancellation".into()
        } else {
            ack.message
        })),
        // `send_command` fails this way when the session died between the
        // lookup and the send, when the socket is closed, or when nobody read
        // the frame before the ACK deadline. All three mean the same thing here
        // as no session at all.
        Err(HubError::Conflict(_) | HubError::Unavailable(_)) => {
            cancel_job_locally(state, record).await
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn cancel_job_locally(
    state: &AppState,
    record: &JobRecord,
) -> Result<JobView, HubError> {
    let job_id = &record.view.id;
    let current = state
        .data
        .read()
        .await
        .jobs
        .get(job_id)
        .cloned()
        .ok_or_else(|| HubError::Conflict("job changed while cancellation was pending".into()))?;
    if current.view.state.is_terminal() {
        return Ok(current.view);
    }
    let now = now_unix_ms();
    let queued = current.view.state == JobState::Queued;
    let message = if queued {
        "cancelled before worker dispatch"
    } else {
        "cancelled while the worker was disconnected"
    };
    if let Some(store) = state.store.as_ref() {
        let cancelled = if queued {
            // Restart hydration makes queued children resident too. Cancelling
            // one must still remove its backlog row and release quota in the
            // same transaction as the state change.
            store
                .cancel_queued_job(&current.organization_id, job_id, now)
                .await?
        } else {
            store
                .update_job_if_current(ConditionalJobUpdate {
                    organization_id: &current.organization_id,
                    id: job_id,
                    attempt: i64::from(current.dispatch.attempt),
                    expected_state: job_state_name(current.view.state),
                    expected_last_event: current.last_event.min(i64::MAX as u64) as i64,
                    state: Some("cancelled"),
                    error: Some(message),
                    now,
                })
                .await?
        };
        if !cancelled {
            return Err(HubError::Conflict(
                "job advanced while cancellation was being committed".into(),
            ));
        }
    }
    if queued {
        info!(%job_id, "cancelled a queued job before worker dispatch");
    } else {
        warn!(
            %job_id,
            worker_id = %current.view.worker_id,
            "cancelled a job whose worker is not connected"
        );
    }
    let mut data = state.data.write().await;
    let matches_snapshot = data.jobs.get(job_id).is_some_and(|job| {
        job.dispatch.attempt == current.dispatch.attempt
            && job.view.state == current.view.state
            && job.last_event == current.last_event
    });
    if !matches_snapshot {
        return Err(HubError::Conflict(
            "job advanced while cancellation was being applied".into(),
        ));
    }
    let mut view = data
        .jobs
        .get(job_id)
        .expect("snapshot match requires a resident job")
        .view
        .clone();
    view.state = JobState::Cancelled;
    view.error = Some(message.into());
    view.updated_at_unix_ms = view.updated_at_unix_ms.max(now);
    if state.store.is_some() {
        data.jobs.remove(job_id);
    } else if let Some(job) = data.jobs.get_mut(job_id) {
        job.view.clone_from(&view);
    }
    drop(data);
    Ok(view)
}

pub(super) fn principal_can_cancel_job(principal: &Principal, record: &JobRecord) -> bool {
    principal.allows(Permission::JobsCancelAny)
        || (principal.allows(Permission::JobsCancelOwn)
            && principal.user_id.as_deref() == record.actor_user_id.as_deref())
}

pub(super) fn principal_kind_name(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::BrowserSession => "browser_session",
        PrincipalKind::ApiKey => "api_key",
        PrincipalKind::WorkerCredential => "worker_credential",
        PrincipalKind::LegacyToken => "legacy_token",
    }
}

pub(super) async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Response {
    if let Err(error) = require_consumer(&headers, &state.config) {
        return api_error(error);
    }
    let principal = Principal {
        kind:            PrincipalKind::LegacyToken,
        actor_id:        "legacy_consumer".into(),
        user_id:         None,
        organization_id: state.config.auth.legacy_organization_id.clone(),
        role:            Role::Owner,
        scopes:          BTreeSet::new(),
    };
    match job_for_principal(&state, &principal, &job_id).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => api_error(error),
    }
}

pub(super) async fn cancel_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Response {
    if let Err(error) = require_consumer(&headers, &state.config) {
        return api_error(error);
    }
    let record = match state.data.read().await.jobs.get(&job_id).cloned() {
        Some(record) if record.organization_id == state.config.auth.legacy_organization_id => {
            record
        }
        None => return api_error(HubError::NotFound("job".into())),
        Some(_) => return api_error(HubError::NotFound("job".into())),
    };
    if record.view.state.is_terminal() || record.view.state == JobState::Uploading {
        return api_error(HubError::Conflict(
            "job cannot be cancelled after output upload begins".into(),
        ));
    }
    match send_job_cancellation(&state, &record).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => api_error(error),
    }
}
