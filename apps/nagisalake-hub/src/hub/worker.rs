use super::*;

pub(super) async fn worker_connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let auth = match authenticate_worker(&state, &headers).await {
        Ok(auth) => auth,
        Err(error) => return api_error(error),
    };
    let transport_config = state.config.transport.clone();
    upgrade
        .protocols([TOKILAKE_SUBPROTOCOL])
        .max_message_size(transport_config.max_frame_bytes)
        .max_frame_size(transport_config.max_frame_bytes)
        .on_upgrade(move |socket| serve_worker(socket, state, transport_config, auth))
        .into_response()
}

#[derive(Debug, Clone)]
pub(super) struct WorkerAuth {
    pub(super) organization_id:   String,
    pub(super) owner_user_id:     Option<String>,
    pub(super) allowed_namespace: Option<String>,
    pub(super) credential_id:     Option<String>,
}

pub(super) async fn authenticate_worker(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<WorkerAuth, HubError> {
    let token = bearer_token(headers)
        .ok_or_else(|| HubError::Unauthorized("worker bearer token is required".into()))?;
    if token.starts_with("nwk_") {
        let store = state.store.as_ref().ok_or_else(|| {
            HubError::Unavailable("PostgreSQL worker credentials are not configured".into())
        })?;
        let credential = store
            .worker_credential_by_hash(&hash_secret(&token))
            .await?
            .ok_or_else(|| HubError::Unauthorized("invalid worker credential".into()))?;
        if credential.revoked_at.is_some()
            || credential
                .expires_at
                .is_some_and(|expires_at| expires_at <= now_unix_ms())
        {
            return Err(HubError::Unauthorized(
                "worker credential is expired or revoked".into(),
            ));
        }
        store
            .touch_worker_credential(&credential.id, now_unix_ms())
            .await?;
        return Ok(WorkerAuth {
            credential_id:     Some(credential.id),
            organization_id:   credential.organization_id,
            owner_user_id:     credential.owner_user_id,
            allowed_namespace: credential.allowed_namespace,
        });
    }
    let expected = state
        .config
        .auth
        .worker_token
        .as_deref()
        .ok_or_else(|| HubError::Unauthorized("invalid worker credential type".into()))?;
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Err(HubError::Unauthorized("invalid worker token".into()));
    }
    Ok(WorkerAuth {
        organization_id:   state.config.auth.legacy_organization_id.clone(),
        owner_user_id:     None,
        allowed_namespace: None,
        credential_id:     None,
    })
}

pub(super) async fn serve_worker(
    socket: axum::extract::ws::WebSocket,
    state: AppState,
    config: TransportConfig,
    auth: WorkerAuth,
) {
    let mut transport = match HubTransport::accept(
        socket,
        config.max_frame_bytes,
        Duration::from_secs(config.accept_timeout_seconds),
    )
    .await
    {
        Ok(transport) => transport,
        Err(error) => {
            warn!(?error, "failed to establish worker SMUX session");
            return;
        }
    };
    let register = match tokio::time::timeout(
        Duration::from_secs(config.accept_timeout_seconds),
        transport.control_mut().receive(),
    )
    .await
    {
        Ok(Ok(Some(WorkerMessage::Register(register)))) => register,
        Ok(Ok(Some(_))) => {
            warn!("worker first control message was not Register");
            return;
        }
        Ok(Ok(None)) => return,
        Ok(Err(error)) => {
            warn!(?error, "worker registration frame failed");
            return;
        }
        Err(_) => {
            warn!("worker registration timed out");
            return;
        }
    };
    if let Err(error) = register.validate() {
        warn!(?error, "worker registration validation failed");
        return;
    }
    if auth
        .allowed_namespace
        .as_deref()
        .is_some_and(|allowed| allowed != register.namespace)
    {
        warn!(
            allowed_namespace = ?auth.allowed_namespace,
            reported_namespace = %register.namespace,
            "worker credential namespace constraint rejected registration"
        );
        return;
    }
    let worker_id = format!(
        "{}/{}",
        safe_component(&register.namespace),
        safe_component(&register.node_name)
    );
    let session_id = Uuid::new_v4().to_string();
    // `String -> FastStr` hands over the existing allocation rather than
    // copying, so these conversions cost no more than the clones they replace
    // and every later clone of the view becomes a refcount bump.
    let view = WorkerView {
        organization_id: auth.organization_id.clone().into(),
        owner_user_id:   auth.owner_user_id.clone().map(FastStr::from),
        worker_id:       worker_id.clone().into(),
        session_id:      session_id.clone().into(),
        namespace:       register.namespace.clone().into(),
        node_name:       register.node_name.clone().into(),
        capabilities:    register.capabilities.clone(),
        active_jobs:     0,
        queued_jobs:     0,
        connected_at:    now_unix_ms(),
    };
    let (outbound, mut outbound_rx) = mpsc::channel(256);
    let replay_sender = outbound.clone();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let disconnect = CancellationToken::new();
    if let Some(store) = state.store.as_ref() {
        let capabilities_json = match serde_json::to_string(&register.capabilities) {
            Ok(value) => value,
            Err(error) => {
                warn!(?error, "failed to serialize worker capabilities");
                return;
            }
        };
        if let Err(error) = store
            .upsert_worker(WorkerUpsert {
                organization_id:   &auth.organization_id,
                id:                &worker_id,
                owner_user_id:     auth.owner_user_id.as_deref(),
                namespace:         &register.namespace,
                node_name:         &register.node_name,
                worker_version:    &register.worker_version,
                capabilities_json: &capabilities_json,
                session_id:        Some(&session_id),
                now:               now_unix_ms(),
            })
            .await
        {
            warn!(?error, %worker_id, "failed to persist worker registration");
            return;
        }
        state
            .invalidate_cached_device_access_for_organization(&auth.organization_id)
            .await;
        for workflow in &register.capabilities.workflows {
            let manifest_json = workflow
                .manifest
                .as_ref()
                .and_then(|manifest| serde_json::to_string(manifest).ok());
            let output_types_json =
                serde_json::to_string(&workflow.output_types).unwrap_or_else(|_| "[]".into());
            let content_hash = hash_secret(&format!(
                "{}:{}",
                manifest_json.as_deref().unwrap_or("null"),
                output_types_json
            ));
            if let Err(error) = store
                .upsert_workflow(WorkflowUpsert {
                    organization_id:   &auth.organization_id,
                    worker_id:         &worker_id,
                    workflow_id:       &workflow.id,
                    version:           &workflow.version,
                    manifest_json:     manifest_json.as_deref(),
                    output_types_json: &output_types_json,
                    content_hash:      Some(&content_hash),
                    now:               now_unix_ms(),
                })
                .await
            {
                warn!(?error, workflow_id = %workflow.id, "failed to persist workflow manifest");
                return;
            }
        }
        // Registration is the full picture of what this worker offers, so drop
        // links to versions it no longer reports. Without this a version renamed
        // in the worker config stays in the catalog forever with no online
        // device, and there is no route to delete it.
        let offered = register
            .capabilities
            .workflows
            .iter()
            .map(|workflow| (workflow.id.clone(), workflow.version.clone()))
            .collect::<Vec<_>>();
        match store
            .retain_worker_workflows(&auth.organization_id, &worker_id, &offered)
            .await
        {
            Ok(0) => {}
            Ok(removed) => {
                info!(
                    %worker_id,
                    removed,
                    "released workflow versions this worker no longer offers"
                );
            }
            // Not fatal: a stale link only means an extra catalog entry, which is
            // better than refusing an otherwise valid registration.
            Err(error) => warn!(?error, %worker_id, "failed to reconcile worker workflows"),
        }
    }
    if !state
        .sessions
        .insert(WorkerSession {
            view,
            credential_id: auth.credential_id.clone(),
            outbound,
            pending: pending.clone(),
            pending_capacity_reservations: HashSet::new(),
            confirmed_capacity_reservations: HashSet::new(),
            disconnect: disconnect.clone(),
            last_seen: Instant::now(),
        })
        .await
    {
        warn!(%worker_id, "worker credential was revoked during registration");
        return;
    }
    let terminal_recovery_job_ids = if let Some(store) = state.store.as_ref() {
        match store
            .terminal_recovery_jobs_for_worker(
                &auth.organization_id,
                &worker_id,
                &register.recovery_job_ids,
            )
            .await
        {
            Ok(job_ids) => job_ids,
            Err(error) => {
                warn!(?error, %worker_id, "failed to reconcile the worker recovery inventory");
                state
                    .sessions
                    .remove_if_current(&auth.organization_id, &worker_id, &session_id)
                    .await;
                return;
            }
        }
    } else {
        Vec::new()
    };
    let replay =
        match rebind_jobs_for_session(&state, &auth.organization_id, &worker_id, &session_id).await
        {
            Ok(replay) => replay,
            Err(error) => {
                warn!(?error, %worker_id, %session_id, "failed to rebuild durable dispatches");
                state
                    .sessions
                    .remove_if_current(&auth.organization_id, &worker_id, &session_id)
                    .await;
                return;
            }
        };
    if transport
        .control_mut()
        .send(&HubMessage::Registered(Registered {
            worker_id:                  worker_id.clone(),
            session_id:                 session_id.clone(),
            heartbeat_interval_seconds: config.heartbeat_interval_seconds,
            server_unix_ms:             now_unix_ms(),
        }))
        .await
        .is_err()
    {
        state
            .sessions
            .remove_if_current(&auth.organization_id, &worker_id, &session_id)
            .await;
        return;
    }
    // This write is deliberately before durable-job replay. The worker starts
    // local journal recovery before it registers, so a Hub-terminal entry may
    // already occupy its only execution slot. Sending an existing CancelJob
    // command here makes the worker persist a terminal local state and release
    // that slot before any recovered live job is redelivered.
    for job_id in terminal_recovery_job_ids {
        let command = CancelJob {
            command_id: Uuid::new_v4().to_string(),
            job_id,
            reason: "Hub already recorded this recovered job as terminal".into(),
        };
        if transport
            .control_mut()
            .send(&HubMessage::CancelJob(command))
            .await
            .is_err()
        {
            state
                .sessions
                .remove_if_current(&auth.organization_id, &worker_id, &session_id)
                .await;
            return;
        }
    }
    for dispatch in replay {
        if state
            .sessions
            .current_session_id(&auth.organization_id, &worker_id)
            .await
            .as_deref()
            != Some(session_id.as_str())
        {
            return;
        }
        let dispatch_job_id = dispatch.job_id.clone();
        let dispatch_attempt = i64::from(dispatch.attempt);
        if replay_sender
            .send(HubMessage::DispatchJob(dispatch))
            .await
            .is_err()
        {
            state
                .sessions
                .remove_if_current(&auth.organization_id, &worker_id, &session_id)
                .await;
            return;
        }
        if state
            .sessions
            .current_session_id(&auth.organization_id, &worker_id)
            .await
            .as_deref()
            != Some(session_id.as_str())
        {
            return;
        }
        if let Some(store) = state.store.as_ref()
            && let Err(error) = store
                .mark_dispatch_delivered(&auth.organization_id, &dispatch_job_id, dispatch_attempt)
                .await
        {
            warn!(?error, %dispatch_job_id, "failed to mark replayed dispatch delivered");
        }
    }
    info!(%worker_id, %session_id, "worker connected");
    loop {
        tokio::select! {
            _ = disconnect.cancelled() => break,
            inbound = transport.control_mut().receive() => {
                match inbound {
                    Ok(Some(message)) => {
                        if let Err(error) = handle_worker_message(
                            &state,
                            &auth.organization_id,
                            &worker_id,
                            &session_id,
                            &pending,
                            message,
                            &mut transport,
                        ).await {
                            warn!(%worker_id, %session_id, ?error, "worker message failed");
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        warn!(%worker_id, %session_id, ?error, "worker control stream failed");
                        break;
                    }
                }
            }
            outbound = outbound_rx.recv() => {
                let Some(message) = outbound else { break };
                if let Err(error) = transport.control_mut().send(&message).await {
                    warn!(%worker_id, %session_id, ?error, "worker outbound frame failed");
                    break;
                }
            }
        }
    }
    state
        .sessions
        .remove_if_current(&auth.organization_id, &worker_id, &session_id)
        .await;
    info!(%worker_id, %session_id, "worker disconnected");
}

pub(super) async fn rebind_jobs_for_session(
    state: &AppState,
    organization_id: &str,
    worker_id: &str,
    session_id: &str,
) -> Result<Vec<DispatchJob>, HubError> {
    let now = now_unix_ms();
    let mut replay = Vec::new();
    // Only the ids under the lock. Cloning whole records here meant holding a
    // deep copy of every in-flight job — parameters, event history and all —
    // for the duration of the presign round trips below.
    let job_ids = state
        .data
        .read()
        .await
        .jobs
        .values()
        .filter(|job| {
            job.worker_organization_id == organization_id
                && job.view.worker_id == worker_id
                && !job.view.state.is_terminal()
        })
        .map(|job| job.view.id.clone())
        .collect::<Vec<_>>();
    for job_id in job_ids {
        let Some(job) = state.data.read().await.jobs.get(&job_id).cloned() else {
            continue;
        };
        let mut artifacts = Vec::with_capacity(job.view.input_artifact_ids.len());
        for artifact_id in &job.view.input_artifact_ids {
            artifacts.push(artifact_record(state, &job.organization_id, artifact_id).await?);
        }
        let mut inputs = Vec::with_capacity(artifacts.len());
        for artifact in artifacts {
            let download = state
                .objects
                .presign_get(&artifact.object_key)
                .await
                .map_err(|error| HubError::ObjectStore(error.to_string()))?;
            inputs.push(JobInput {
                artifact_id: artifact.view.id,
                name: artifact.view.name,
                content_type: artifact.view.content_type,
                size_bytes: artifact.view.size_bytes,
                sha256: artifact.view.sha256,
                download,
            });
        }
        if state
            .sessions
            .current_session_id(organization_id, worker_id)
            .await
            .as_deref()
            != Some(session_id)
        {
            break;
        }
        if let Some(store) = state.store.as_ref() {
            let rebound = store
                .rebind_job_session(
                    &job.organization_id,
                    &job.view.id,
                    i64::from(job.dispatch.attempt),
                    &job.view.session_id,
                    session_id,
                    now,
                )
                .await?;
            if !rebound {
                if state
                    .sessions
                    .current_session_id(organization_id, worker_id)
                    .await
                    .as_deref()
                    != Some(session_id)
                {
                    break;
                }
                let Some(stored) = store.job(&job.organization_id, &job.view.id).await? else {
                    continue;
                };
                let Some(stored_session_id) = stored.session_id.as_deref() else {
                    continue;
                };
                if !store
                    .rebind_job_session(
                        &job.organization_id,
                        &job.view.id,
                        i64::from(job.dispatch.attempt),
                        stored_session_id,
                        session_id,
                        now,
                    )
                    .await?
                {
                    continue;
                }
            }
        }
        let Some(_session_guard) = state
            .sessions
            .guard_current_session(organization_id, worker_id, session_id)
            .await
        else {
            break;
        };
        let mut data = state.data.write().await;
        let Some(current) = data.jobs.get_mut(&job_id) else {
            continue;
        };
        if current.worker_organization_id != organization_id
            || current.view.worker_id != worker_id
            || current.dispatch.attempt != job.dispatch.attempt
            || current.view.state.is_terminal()
        {
            continue;
        }
        session_id.clone_into(&mut current.view.session_id);
        current.view.updated_at_unix_ms = current.view.updated_at_unix_ms.max(now);
        current.dispatch.command_id = Uuid::new_v4().to_string();
        current.dispatch.inputs = inputs;
        replay.push(current.dispatch.clone());
    }
    Ok(replay)
}

pub(super) async fn handle_worker_message(
    state: &AppState,
    organization_id: &str,
    worker_id: &str,
    session_id: &str,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<CommandAck>>>>,
    message: WorkerMessage,
    transport: &mut HubTransport,
) -> Result<(), HubError> {
    match message {
        WorkerMessage::Register(_) => {
            Err(HubError::Conflict("worker is already registered".into()))
        }
        WorkerMessage::Heartbeat(Heartbeat {
            session_id: reported,
            active_jobs,
            queued_jobs,
            ..
        }) => {
            if reported != session_id {
                return Err(HubError::Conflict(
                    "heartbeat session does not match socket".into(),
                ));
            }
            let result = state
                .sessions
                .update_heartbeat(
                    organization_id,
                    worker_id,
                    session_id,
                    active_jobs,
                    queued_jobs,
                )
                .await;
            if result.is_ok()
                && let Some(store) = state.store.as_ref()
                && let Err(error) = store
                    .touch_worker_heartbeat(organization_id, worker_id, session_id, now_unix_ms())
                    .await
            {
                // In-memory liveness remains authoritative for the live socket;
                // a transient database error must not disconnect a healthy worker.
                warn!(?error, %worker_id, "failed to persist worker heartbeat");
            }
            result
        }
        WorkerMessage::CommandAck(ack) => {
            state
                .sessions
                .settle_capacity_reservation(
                    organization_id,
                    worker_id,
                    session_id,
                    &ack.command_id,
                    ack.accepted,
                )
                .await;
            if let Some(sender) = pending.lock().await.remove(&ack.command_id) {
                let _ = sender.send(ack);
            }
            Ok(())
        }
        WorkerMessage::JobEvent(event) => {
            let ack = apply_job_event(state, organization_id, worker_id, session_id, event.clone())
                .await?;
            transport
                .control_mut()
                .send(&HubMessage::JobEventAck(ack))
                .await
                .map_err(HubError::Transport)
        }
        WorkerMessage::ArtifactReady(ready) => {
            let upload =
                prepare_artifact_upload(state, organization_id, worker_id, session_id, ready)
                    .await?;
            transport
                .control_mut()
                .send(&HubMessage::ArtifactUpload(upload))
                .await
                .map_err(HubError::Transport)
        }
        WorkerMessage::ArtifactUploaded(uploaded) => {
            let ack =
                complete_artifact_upload(state, organization_id, worker_id, session_id, uploaded)
                    .await?;
            transport
                .control_mut()
                .send(&HubMessage::ArtifactUploadedAck(ack))
                .await
                .map_err(HubError::Transport)
        }
        WorkerMessage::Pong(_) => Ok(()),
    }
}

/// How many events stay resident per job. Older ones live in the store only.
pub(super) const MAX_RESIDENT_JOB_EVENTS: usize = 256;

fn can_recover_forward_transition(current: JobState, next: JobState) -> bool {
    matches!(
        (current, next),
        (
            JobState::Received,
            JobState::Running | JobState::Uploading | JobState::Completed
        ) | (
            JobState::Accepted,
            JobState::Uploading | JobState::Completed
        ) | (JobState::Running, JobState::Completed)
    )
}

async fn merge_adopted_job_session(
    state: &AppState,
    organization_id: &str,
    worker_id: &str,
    job_id: &str,
    expected_attempt: u32,
    session_id: &str,
    now: i64,
) {
    let Some(_session_guard) = state
        .sessions
        .guard_current_session(organization_id, worker_id, session_id)
        .await
    else {
        return;
    };
    let mut data = state.data.write().await;
    let Some(job) = data.jobs.get_mut(job_id) else {
        return;
    };
    if job.dispatch.attempt != expected_attempt || job.view.state.is_terminal() {
        return;
    }
    session_id.clone_into(&mut job.view.session_id);
    job.view.updated_at_unix_ms = job.view.updated_at_unix_ms.max(now);
}

async fn reconcile_persisted_job_event(
    state: &AppState,
    store: &PgStore,
    organization_id: &str,
    job_id: &str,
    sequence: u64,
) -> Result<bool, HubError> {
    let Some(stored) = store.job(organization_id, job_id).await? else {
        return Ok(false);
    };
    let persisted_state = parse_job_state(&stored.state)?;
    let persisted_last_event = u64::try_from(stored.last_event)
        .map_err(|_| HubError::InvalidConfig("persisted job sequence is invalid".into()))?;
    if persisted_state.is_terminal() {
        store
            .release_job_for_terminal(organization_id, job_id)
            .await?;
        let mut data = state.data.write().await;
        data.jobs.remove(job_id);
        data.artifacts
            .retain(|_id, artifact| artifact.view.job_id.as_deref() != Some(job_id));
        return Ok(true);
    }
    if persisted_last_event < sequence {
        return Ok(false);
    }
    let events = store.events_for_job(organization_id, job_id).await?;
    let durable_view = job_view_from_stored(stored, &events)?;
    let mut data = state.data.write().await;
    if let Some(current) = data.jobs.get_mut(job_id) {
        if current.last_event > persisted_last_event
            || (current.last_event == persisted_last_event
                && (current.view.state == durable_view.state
                    || (!current.view.state.can_transition_to(durable_view.state)
                        && !can_recover_forward_transition(
                            current.view.state,
                            durable_view.state,
                        ))))
        {
            return Ok(true);
        }
        current.view.state = durable_view.state;
        current.view.progress = durable_view.progress;
        current.view.prompt_id = durable_view.prompt_id;
        current.view.error = durable_view.error;
        current.view.events = durable_view.events;
        current.view.updated_at_unix_ms = current
            .view
            .updated_at_unix_ms
            .max(durable_view.updated_at_unix_ms);
        current.last_event = persisted_last_event;
    }
    Ok(true)
}

pub(super) async fn apply_job_event(
    state: &AppState,
    organization_id: &str,
    worker_id: &str,
    session_id: &str,
    event: JobEvent,
) -> Result<JobEventAck, HubError> {
    let ack = JobEventAck {
        job_id:   event.job_id.clone(),
        sequence: event.sequence,
    };
    let Some(mut updated) = state.data.read().await.jobs.get(&event.job_id).cloned() else {
        // Terminal jobs are evicted from memory once persisted, so a miss here
        // is usually a worker replaying the very event that finished the job:
        // its first ack was lost. Answering NotFound would make it retry that
        // frame for as long as it stays connected, so confirm what the store
        // already recorded and let the worker move on.
        if let Some(store) = state.store.as_ref()
            && reconcile_persisted_job_event(
                state,
                store,
                organization_id,
                &event.job_id,
                event.sequence,
            )
            .await?
        {
            return Ok(ack);
        }
        return Err(HubError::NotFound("job".into()));
    };
    if updated.worker_organization_id != organization_id
        || updated.view.worker_id != worker_id
        || event.attempt != updated.dispatch.attempt
    {
        debug!(job_id = %event.job_id, "ignored event from a worker that does not own this job");
        return Ok(ack);
    }
    if state
        .sessions
        .current_session_id(organization_id, worker_id)
        .await
        .as_deref()
        != Some(session_id)
    {
        debug!(job_id = %event.job_id, "rejected event from a stale worker session");
        return Err(HubError::Conflict("stale worker session".into()));
    }
    // The recorded session id lags behind reconnects: `rebind_jobs_for_session`
    // only rewrites jobs resident when the new socket registers, and a restart
    // hydrates whatever id the row held beforehand. Dropping the event in that
    // window was permanent, because the ack this function returns makes the
    // worker clear its pending event and never send it again. Adopt the sender
    // when it holds the worker's live session instead; only a socket that has
    // genuinely been superseded is still ignored.
    let previous_session_id = updated.view.session_id.clone();
    let mut session_adopted_from = None;
    if updated.view.session_id != session_id {
        info!(
            job_id = %event.job_id,
            previous_session = %updated.view.session_id,
            %session_id,
            "adopted job into the worker's current session"
        );
        session_adopted_from = Some(std::mem::replace(
            &mut updated.view.session_id,
            session_id.to_owned(),
        ));
    }
    let next_state = match event.kind {
        JobEventKind::Accepted => JobState::Accepted,
        JobEventKind::Running | JobEventKind::Progress => JobState::Running,
        JobEventKind::Uploading => JobState::Uploading,
        JobEventKind::Completed => JobState::Completed,
        JobEventKind::Failed => JobState::Failed,
        JobEventKind::Cancelled => JobState::Cancelled,
    };
    let previous_state = updated.view.state;
    let previous_last_event = updated.last_event;
    let mut apply_event = false;
    let mut append_event = false;
    if event.sequence <= updated.last_event {
        // A worker retries an event until it is acked, so the same sequence
        // arrives more than once. Skipping the duplicate row is right; skipping
        // the state the event carries is not. Older rows may already have an
        // event stream ahead of the job state because event and job persistence
        // were previously separate writes. Skipping unconditionally made that
        // permanent: every retry looked like a duplicate, leaving jobs parked in
        // `received` with an `accepted` event on record and no way out but an
        // operator.
        //
        // Re-applying is safe because an event names the state it implies, so
        // applying it twice lands on the same value.
        if updated.view.state != next_state
            && (updated.view.state.can_transition_to(next_state)
                || can_recover_forward_transition(updated.view.state, next_state))
        {
            warn!(
                job_id = %event.job_id,
                sequence = event.sequence,
                last_event = updated.last_event,
                from = job_state_name(updated.view.state),
                to = job_state_name(next_state),
                "advanced a job whose state lagged its own event stream"
            );
            apply_event = true;
        }
    } else if !updated.view.state.can_transition_to(next_state)
        && !can_recover_forward_transition(updated.view.state, next_state)
    {
        warn!(
            job_id = %event.job_id,
            sequence = event.sequence,
            from = job_state_name(updated.view.state),
            to = job_state_name(next_state),
            "rejected a new event with an invalid transition without acknowledging it"
        );
        return Err(HubError::Conflict(format!(
            "invalid event transition from {} to {}",
            job_state_name(updated.view.state),
            job_state_name(next_state)
        )));
    } else {
        if !updated.view.state.can_transition_to(next_state) {
            warn!(
                job_id = %event.job_id,
                sequence = event.sequence,
                from = job_state_name(updated.view.state),
                to = job_state_name(next_state),
                "recovered a forward event after the cached state fell behind"
            );
        }
        apply_event = true;
        append_event = true;
    }
    if !apply_event {
        if let Some(previous_session_id) = session_adopted_from.as_deref() {
            let now = now_unix_ms();
            if let Some(store) = state.store.as_ref()
                && !store
                    .rebind_job_session(
                        &updated.organization_id,
                        &updated.view.id,
                        i64::from(updated.dispatch.attempt),
                        previous_session_id,
                        session_id,
                        now,
                    )
                    .await?
            {
                return Err(HubError::Conflict(
                    "job changed while adopting the worker session".into(),
                ));
            }
            merge_adopted_job_session(
                state,
                organization_id,
                worker_id,
                &event.job_id,
                updated.dispatch.attempt,
                session_id,
                now,
            )
            .await;
        }
        return Ok(ack);
    }
    let event_error = if matches!(event.kind, JobEventKind::Failed | JobEventKind::Cancelled) {
        (!event.message.is_empty()).then(|| event.message.clone())
    } else {
        None
    };
    let now = now_unix_ms();
    updated.view.state = next_state;
    updated.view.progress = event.progress.or(updated.view.progress);
    updated.view.prompt_id = event.prompt_id.clone().or(updated.view.prompt_id.take());
    updated.view.error.clone_from(&event_error);
    updated.view.updated_at_unix_ms = now;
    if append_event {
        updated.last_event = event.sequence;
        updated.view.events.push(JobEventView {
            sequence: event.sequence,
            kind:     event.kind,
            progress: event.progress,
            message:  event.message.clone(),
            unix_ms:  event.unix_ms,
        });
        if updated.view.events.len() > MAX_RESIDENT_JOB_EVENTS {
            let remove = updated.view.events.len() - MAX_RESIDENT_JOB_EVENTS;
            updated.view.events.drain(..remove);
        }
    }
    if let Some(store) = state.store.as_ref() {
        let applied = store
            .apply_job_event(
                EventInsert {
                    organization_id: &updated.organization_id,
                    job_id: &event.job_id,
                    attempt: i64::from(event.attempt),
                    sequence: event.sequence.min(i64::MAX as u64) as i64,
                    kind: event_kind_name(event.kind),
                    progress: event.progress,
                    prompt_id: event.prompt_id.as_deref(),
                    message: &event.message,
                    unix_ms: event.unix_ms,
                    now,
                },
                JobEventUpdate {
                    session_id,
                    expected_session_id: &previous_session_id,
                    expected_state: job_state_name(previous_state),
                    expected_last_event: previous_last_event.min(i64::MAX as u64) as i64,
                    state: job_state_name(next_state),
                    error: event_error.as_deref(),
                },
            )
            .await?;
        if !applied {
            if reconcile_persisted_job_event(
                state,
                store,
                organization_id,
                &event.job_id,
                event.sequence,
            )
            .await?
            {
                return Ok(ack);
            }
            return Err(HubError::Conflict(
                "job changed while applying an event; retry the event".into(),
            ));
        }
    }
    let now_terminal = updated.view.state.is_terminal();
    // The job is persisted above, so once it is terminal it no longer needs to
    // stay resident. Keeping it would make memory grow with total history again
    // between restarts, which is exactly what trimming hydration avoids. Reads
    // fall back to the store; see `job_for_principal`.
    //
    // Only drop it when the store is available: without a control plane the
    // in-memory map is the only copy.
    let current_session_guard = state
        .sessions
        .guard_current_session(organization_id, worker_id, session_id)
        .await;
    let mut data = state.data.write().await;
    let cache_matches = data.jobs.get(&event.job_id).is_some_and(|current| {
        current.dispatch.attempt == event.attempt
            && current.view.state == previous_state
            && current.last_event == previous_last_event
    });
    if cache_matches {
        if now_terminal && state.store.is_some() {
            data.jobs.remove(&event.job_id);
            data.artifacts.retain(|_id, artifact| {
                artifact.view.job_id.as_deref() != Some(event.job_id.as_str())
            });
        } else if let Some(current) = data.jobs.get_mut(&event.job_id) {
            if current_session_guard.is_some() && current.view.session_id == previous_session_id {
                session_id.clone_into(&mut current.view.session_id);
            }
            current.view.state = updated.view.state;
            current.view.progress = updated.view.progress;
            current.view.prompt_id.clone_from(&updated.view.prompt_id);
            current.view.error.clone_from(&updated.view.error);
            current.view.updated_at_unix_ms = current
                .view
                .updated_at_unix_ms
                .max(updated.view.updated_at_unix_ms);
            current.last_event = updated.last_event;
            if append_event
                && !current
                    .view
                    .events
                    .iter()
                    .any(|entry| entry.sequence == event.sequence)
            {
                current.view.events.push(
                    updated
                        .view
                        .events
                        .last()
                        .expect("event was appended")
                        .clone(),
                );
                if current.view.events.len() > MAX_RESIDENT_JOB_EVENTS {
                    let remove = current.view.events.len() - MAX_RESIDENT_JOB_EVENTS;
                    current.view.events.drain(..remove);
                }
            }
        }
    }
    drop(data);
    Ok(ack)
}

pub(super) async fn prepare_artifact_upload(
    state: &AppState,
    worker_organization_id: &str,
    worker_id: &str,
    session_id: &str,
    ready: ArtifactReady,
) -> Result<ArtifactUpload, HubError> {
    if ready.size_bytes == 0 || ready.size_bytes > state.config.transport.max_artifact_bytes {
        return Err(HubError::InvalidRequest(
            "output exceeds the configured size limit".into(),
        ));
    }
    if !is_sha256(&ready.sha256) || ready.content_type.trim().is_empty() {
        return Err(HubError::InvalidRequest(
            "output metadata is invalid".into(),
        ));
    }
    let (job_id, artifact_organization_id, name) = {
        let data = state.data.read().await;
        let Some(job) = data.jobs.get(&ready.job_id) else {
            return Err(HubError::NotFound("job".into()));
        };
        if job.worker_organization_id != worker_organization_id
            || job.view.worker_id != worker_id
            || job.view.session_id != session_id
            || job.dispatch.attempt != ready.attempt
        {
            return Err(HubError::Conflict("stale worker session".into()));
        }
        (
            job.view.id.clone(),
            job.organization_id.clone(),
            safe_filename(&ready.name),
        )
    };
    let pending_upload = {
        let data = state.data.read().await;
        data.pending_uploads
            .get(&pending_upload_key(
                &artifact_organization_id,
                &ready.request_id,
            ))
            .cloned()
            .map(|existing_id| {
                let existing = data.artifacts.get(&existing_id).cloned();
                (existing_id, existing)
            })
    };
    if let Some((existing_id, existing)) = pending_upload {
        let existing = existing.ok_or_else(|| HubError::NotFound("artifact".into()))?;
        if existing.organization_id != artifact_organization_id
            || existing.view.job_id.as_deref() != Some(ready.job_id.as_str())
            || existing.view.size_bytes != ready.size_bytes
            || existing.view.content_type != ready.content_type
            || !existing.view.sha256.eq_ignore_ascii_case(&ready.sha256)
        {
            return Err(HubError::Conflict(
                "upload request metadata changed on retry".into(),
            ));
        }
        let upload = state
            .objects
            .presign_put(
                &existing.object_key,
                &existing.view.content_type,
                existing.view.size_bytes,
                &existing.view.sha256,
            )
            .await
            .map_err(|error| HubError::ObjectStore(error.to_string()))?;
        return Ok(ArtifactUpload {
            request_id: ready.request_id,
            artifact_id: existing_id,
            upload,
        });
    }
    let artifact_id = Uuid::new_v4().to_string();
    let object_key =
        format!("organizations/{artifact_organization_id}/outputs/{job_id}/{artifact_id}/{name}");
    if state.store.is_some() {
        state
            .reserve_storage(
                &artifact_organization_id,
                ready.size_bytes.min(i64::MAX as u64) as i64,
            )
            .await
            .map_err(map_store_error)?;
    }
    let upload = state
        .objects
        .presign_put(
            &object_key,
            &ready.content_type,
            ready.size_bytes,
            &ready.sha256,
        )
        .await;
    let upload = match upload {
        Ok(upload) => upload,
        Err(error) => {
            let _ = state
                .release_storage(
                    &artifact_organization_id,
                    ready.size_bytes.min(i64::MAX as u64) as i64,
                )
                .await;
            return Err(HubError::ObjectStore(error.to_string()));
        }
    };
    let view = ArtifactView {
        id: artifact_id.clone(),
        job_id: Some(job_id.clone()),
        name,
        content_type: ready.content_type,
        size_bytes: ready.size_bytes,
        sha256: ready.sha256.to_ascii_lowercase(),
        state: ArtifactState::PendingUpload,
    };
    let record = ArtifactRecord {
        organization_id: artifact_organization_id.clone(),
        view,
        object_key,
    };
    if let Err(error) = persist_artifact(state, &record).await {
        let _ = state
            .release_storage(
                &artifact_organization_id,
                ready.size_bytes.min(i64::MAX as u64) as i64,
            )
            .await;
        return Err(error);
    }
    if let Some(store) = state.store.as_ref()
        && let Err(error) = store
            .upsert_upload_request(UploadRequestUpsert {
                organization_id: &artifact_organization_id,
                request_id:      &ready.request_id,
                artifact_id:     &artifact_id,
                job_id:          Some(&job_id),
                attempt:         Some(i64::from(ready.attempt)),
                now:             now_unix_ms(),
            })
            .await
    {
        let _ = state
            .release_storage(
                &artifact_organization_id,
                ready.size_bytes.min(i64::MAX as u64) as i64,
            )
            .await;
        return Err(HubError::Store(error));
    }
    let mut data = state.data.write().await;
    data.pending_uploads.insert(
        pending_upload_key(&artifact_organization_id, &ready.request_id),
        artifact_id.clone(),
    );
    data.artifacts.insert(artifact_id.clone(), record);
    Ok(ArtifactUpload {
        request_id: ready.request_id,
        artifact_id,
        upload,
    })
}

pub(super) async fn complete_artifact_upload(
    state: &AppState,
    worker_organization_id: &str,
    worker_id: &str,
    session_id: &str,
    uploaded: ArtifactUploaded,
) -> Result<ArtifactUploadedAck, HubError> {
    let artifact_organization_id = {
        let data = state.data.read().await;
        let job = data
            .jobs
            .get(&uploaded.job_id)
            .ok_or_else(|| HubError::NotFound("job".into()))?;
        if job.worker_organization_id != worker_organization_id
            || job.view.worker_id != worker_id
            || job.view.session_id != session_id
            || job.dispatch.attempt != uploaded.attempt
        {
            return Err(HubError::Conflict("stale worker session".into()));
        }
        job.organization_id.clone()
    };
    let artifact_id = {
        let data = state.data.read().await;
        data.pending_uploads
            .get(&pending_upload_key(
                &artifact_organization_id,
                &uploaded.request_id,
            ))
            .cloned()
    };
    let artifact_id = match artifact_id {
        Some(id) => id,
        None => match state.store.as_ref() {
            Some(store) => store
                .upload_request_artifact(&artifact_organization_id, &uploaded.request_id)
                .await?
                .ok_or_else(|| HubError::NotFound("upload request".into()))?,
            None => return Err(HubError::NotFound("upload request".into())),
        },
    };
    if artifact_id != uploaded.artifact_id {
        return Err(HubError::Conflict(
            "artifact id does not match upload request".into(),
        ));
    }
    // A completed upload may be replayed after its ACK was lost and the Hub
    // restarted. Ready artifacts are not hydrated, so fall back to the store.
    let artifact = artifact_record(state, &artifact_organization_id, &artifact_id).await?;
    if artifact.organization_id != artifact_organization_id {
        return Err(HubError::NotFound("artifact".into()));
    }
    let job_id = artifact
        .view
        .job_id
        .clone()
        .ok_or_else(|| HubError::Conflict("output artifact has no job".into()))?;
    if uploaded.job_id != job_id {
        return Err(HubError::Conflict(
            "uploaded artifact job does not match".into(),
        ));
    }
    {
        let data = state.data.read().await;
        let job = data
            .jobs
            .get(&job_id)
            .ok_or_else(|| HubError::NotFound("job".into()))?;
        if job.organization_id != artifact_organization_id
            || job.worker_organization_id != worker_organization_id
            || job.view.worker_id != worker_id
            || job.view.session_id != session_id
            || job.dispatch.attempt != uploaded.attempt
        {
            return Err(HubError::Conflict("stale worker session".into()));
        }
    }
    let metadata = state
        .objects
        .head(&artifact.object_key)
        .await
        .map_err(|error| HubError::ObjectStore(error.to_string()))?;
    if !object_metadata_matches(&metadata, &artifact.view) {
        return Err(HubError::Conflict(
            "uploaded output failed size, content type, or SHA-256 verification".into(),
        ));
    }
    let mut completed_artifact = artifact.clone();
    completed_artifact.view.state = ArtifactState::Ready;
    let now = now_unix_ms();
    if let Some(store) = state.store.as_ref()
        && !store
            .complete_job_output_upload(CompleteJobOutputUpload {
                organization_id: &artifact_organization_id,
                request_id: &uploaded.request_id,
                artifact_id: &artifact_id,
                job_id: &job_id,
                attempt: i64::from(uploaded.attempt),
                session_id,
                now,
            })
            .await?
    {
        return Err(HubError::Conflict(
            "job changed while completing the output artifact".into(),
        ));
    }
    state
        .invalidate_cached_artifact(&artifact_organization_id, &artifact_id)
        .await;
    let mut data = state.data.write().await;
    data.pending_uploads.insert(
        pending_upload_key(&artifact_organization_id, &uploaded.request_id),
        artifact_id.clone(),
    );
    data.artifacts
        .insert(artifact_id.clone(), completed_artifact);
    if let Some(job) = data.jobs.get_mut(&job_id)
        && job.dispatch.attempt == uploaded.attempt
        && !job.view.output_artifact_ids.contains(&artifact_id)
    {
        job.view.output_artifact_ids.push(artifact_id.clone());
        job.view.updated_at_unix_ms = job.view.updated_at_unix_ms.max(now);
    }
    Ok(ArtifactUploadedAck {
        request_id: uploaded.request_id,
        artifact_id,
    })
}

pub(super) async fn mark_job_failed(state: &AppState, job_id: &str, message: String) {
    update_job_error(state, job_id, message, true).await;
}

pub(super) async fn record_dispatch_error(state: &AppState, job_id: &str, message: String) {
    update_job_error(state, job_id, message, false).await;
}

pub(super) async fn update_job_error(
    state: &AppState,
    job_id: &str,
    message: String,
    terminal: bool,
) {
    let Some(job) = state.data.read().await.jobs.get(job_id).cloned() else {
        return;
    };
    if job.view.state.is_terminal() {
        return;
    }
    let now = now_unix_ms();
    let error_message = truncate(&message, 1_000);
    if let Some(store) = state.store.as_ref() {
        let updated = store
            .update_job_if_current(ConditionalJobUpdate {
                organization_id: &job.organization_id,
                id: job_id,
                attempt: i64::from(job.dispatch.attempt),
                expected_state: job_state_name(job.view.state),
                expected_last_event: job.last_event.min(i64::MAX as u64) as i64,
                state: terminal.then_some("failed"),
                error: Some(&error_message),
                now,
            })
            .await;
        match updated {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                warn!(?error, %job_id, "failed to persist job error");
                return;
            }
        }
    }
    let mut data = state.data.write().await;
    let matches_snapshot = data.jobs.get(job_id).is_some_and(|current| {
        current.dispatch.attempt == job.dispatch.attempt
            && current.view.state == job.view.state
            && current.last_event == job.last_event
    });
    if !matches_snapshot {
        return;
    }
    if terminal && state.store.is_some() {
        data.jobs.remove(job_id);
        return;
    }
    if let Some(current) = data.jobs.get_mut(job_id) {
        if terminal {
            current.view.state = JobState::Failed;
        }
        current.view.error = Some(error_message);
        current.view.updated_at_unix_ms = current.view.updated_at_unix_ms.max(now);
    }
}
