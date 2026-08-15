use super::*;

#[derive(Debug, Deserialize)]
pub struct CreateUploadRequest {
    pub name:         String,
    pub content_type: String,
    pub size_bytes:   u64,
    pub sha256:       String,
}

#[derive(Debug, Serialize)]
pub struct CreateUploadResponse {
    pub artifact: ArtifactView,
    pub upload:   nagisalake_protocol::PresignedRequest,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SubmitJobRequest {
    pub workflow_id:            String,
    pub workflow_version:       String,
    #[serde(default)]
    pub parameters:             JsonValue,
    #[serde(default)]
    pub input_artifact_ids:     Vec<String>,
    #[serde(default)]
    pub device_organization_id: Option<String>,
    #[serde(default)]
    pub device_id:              Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct HealthResponse {
    pub(super) status:            &'static str,
    pub(super) connected_workers: usize,
    pub(super) database:          &'static str,
    pub(super) object_storage:    &'static str,
    pub(super) ready:             bool,
}

pub(super) async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(health_snapshot(&state).await)
}

pub(super) async fn ready(State(state): State<AppState>) -> Response {
    let snapshot = health_snapshot(&state).await;
    let ready = snapshot.ready;
    let mut snapshot = snapshot;
    snapshot.status = if ready { "ready" } else { "not_ready" };
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(snapshot)).into_response()
}

pub(super) async fn health_snapshot(state: &AppState) -> HealthResponse {
    let database_check = async {
        match state.store.as_ref() {
            Some(store) => match tokio::time::timeout(Duration::from_secs(2), store.ping()).await {
                Ok(Ok(())) => "ok",
                _ => "error",
            },
            None => "not_configured",
        }
    };
    let object_storage_check = async {
        if !state.objects.is_enabled() {
            "not_configured"
        } else {
            match tokio::time::timeout(Duration::from_secs(2), state.objects.health_check()).await {
                Ok(Ok(())) => "ok",
                _ => "error",
            }
        }
    };
    let (database, object_storage) = tokio::join!(database_check, object_storage_check);
    HealthResponse {
        status: if database == "error" || object_storage == "error" {
            "degraded"
        } else {
            "ok"
        },
        connected_workers: state.sessions.count().await,
        database,
        object_storage,
        ready: database == "ok" && object_storage == "ok",
    }
}

pub(super) async fn metrics_endpoint(State(state): State<AppState>) -> Response {
    let body = state
        .metrics
        .render(state.sessions.count().await, state.store.as_ref());
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

pub(super) async fn list_workers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_consumer(&headers, &state.config) {
        return api_error(error);
    }
    Json(
        state
            .sessions
            .list_for_org(&state.config.auth.legacy_organization_id)
            .await,
    )
    .into_response()
}

pub(super) async fn list_workflows(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_consumer(&headers, &state.config) {
        return api_error(error);
    }
    Json(aggregate_workflows(
        state
            .sessions
            .list_for_org(&state.config.auth.legacy_organization_id)
            .await,
    ))
    .into_response()
}

pub(super) fn aggregate_workflows(workers: Vec<WorkerView>) -> Vec<WorkflowView> {
    let mut workflows = BTreeMap::<(String, String), WorkflowView>::new();
    for worker in workers {
        let availability = WorkflowWorkerView {
            organization_id: worker.organization_id,
            worker_id:       worker.worker_id,
            session_id:      worker.session_id,
            labels:          worker.capabilities.labels,
            parallelism:     worker.capabilities.parallelism,
            queue_depth:     worker.capabilities.queue_depth,
            active_jobs:     worker.active_jobs,
            queued_jobs:     worker.queued_jobs,
            available:       u32::from(worker.active_jobs) + u32::from(worker.queued_jobs)
                < u32::from(worker.capabilities.parallelism)
                    + u32::from(worker.capabilities.queue_depth),
        };
        for capability in worker.capabilities.workflows {
            let entry = workflows
                .entry((capability.id.clone(), capability.version.clone()))
                .or_insert_with(|| WorkflowView {
                    id:                  capability.id,
                    version:             capability.version,
                    output_types:        capability.output_types.clone(),
                    manifest:            capability.manifest.clone(),
                    manifest_consistent: true,
                    workers:             Vec::new(),
                });
            if entry.output_types != capability.output_types
                || entry.manifest != capability.manifest
            {
                entry.manifest_consistent = false;
            }
            entry.workers.push(availability.clone());
        }
    }
    for workflow in workflows.values_mut() {
        workflow
            .workers
            .sort_by(|left, right| left.worker_id.cmp(&right.worker_id));
    }
    workflows.into_values().collect()
}

pub(super) async fn create_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateUploadRequest>,
) -> Response {
    if let Err(error) = require_consumer(&headers, &state.config) {
        return api_error(error);
    }
    if let Err(error) = validate_artifact_metadata(
        &request.name,
        &request.content_type,
        request.size_bytes,
        &request.sha256,
        state.config.transport.max_artifact_bytes,
    ) {
        return api_error(error);
    }
    let artifact_id = Uuid::new_v4().to_string();
    let organization_id = &state.config.auth.legacy_organization_id;
    if state.store.is_some()
        && let Err(error) = state
            .reserve_storage(
                organization_id,
                request.size_bytes.min(i64::MAX as u64) as i64,
            )
            .await
    {
        return api_error(map_store_error(error));
    }
    let object_key = format!(
        "organizations/{organization_id}/inputs/{artifact_id}/{}",
        safe_filename(&request.name)
    );
    let upload = match state
        .objects
        .presign_put(
            &object_key,
            &request.content_type,
            request.size_bytes,
            &request.sha256,
        )
        .await
    {
        Ok(upload) => upload,
        Err(error) => {
            let _ = state
                .release_storage(
                    organization_id,
                    request.size_bytes.min(i64::MAX as u64) as i64,
                )
                .await;
            return api_error(HubError::ObjectStore(error.to_string()));
        }
    };
    let view = ArtifactView {
        id:           artifact_id.clone(),
        job_id:       None,
        name:         request.name,
        content_type: request.content_type,
        size_bytes:   request.size_bytes,
        sha256:       request.sha256.to_ascii_lowercase(),
        state:        ArtifactState::PendingUpload,
    };
    let record = ArtifactRecord {
        organization_id: state.config.auth.legacy_organization_id.clone(),
        view: view.clone(),
        object_key,
    };
    if let Err(error) = persist_artifact(&state, &record).await {
        let _ = state
            .release_storage(
                organization_id,
                request.size_bytes.min(i64::MAX as u64) as i64,
            )
            .await;
        return api_error(error);
    }
    state
        .data
        .write()
        .await
        .artifacts
        .insert(artifact_id, record);
    (
        StatusCode::CREATED,
        Json(CreateUploadResponse {
            artifact: view,
            upload,
        }),
    )
        .into_response()
}

pub(super) async fn complete_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(artifact_id): AxumPath<String>,
    Json(request): Json<CompleteUploadRequest>,
) -> Response {
    if let Err(error) = require_consumer(&headers, &state.config) {
        return api_error(error);
    }
    if artifact_id != request.artifact_id {
        return api_error(HubError::InvalidRequest(
            "artifact id does not match path".into(),
        ));
    }
    let artifact = match state.data.read().await.artifacts.get(&artifact_id).cloned() {
        Some(artifact) if artifact.organization_id == state.config.auth.legacy_organization_id => {
            artifact
        }
        None => return api_error(HubError::NotFound("artifact".into())),
        Some(_) => return api_error(HubError::NotFound("artifact".into())),
    };
    if artifact.view.state != ArtifactState::PendingUpload {
        return api_error(HubError::Conflict("artifact is already completed".into()));
    }
    let metadata = match state.objects.head(&artifact.object_key).await {
        Ok(metadata) => metadata,
        Err(error) => return api_error(HubError::ObjectStore(error.to_string())),
    };
    if metadata.size_bytes != request.size_bytes
        || request.size_bytes != artifact.view.size_bytes
        || !request.sha256.eq_ignore_ascii_case(&artifact.view.sha256)
        || !object_metadata_matches(&metadata, &artifact.view)
    {
        return api_error(HubError::Conflict(
            "object size, content type, or SHA-256 does not match the upload declaration".into(),
        ));
    }
    if let Some(store) = state.store.as_ref()
        && let Err(error) = store
            .set_artifact_state(
                &artifact.organization_id,
                &artifact_id,
                "ready",
                now_unix_ms(),
            )
            .await
    {
        return api_error(HubError::Store(error));
    }
    state
        .invalidate_cached_artifact(&artifact.organization_id, &artifact_id)
        .await;
    let mut data = state.data.write().await;
    let Some(artifact) = data.artifacts.get_mut(&artifact_id) else {
        return api_error(HubError::NotFound("artifact".into()));
    };
    artifact.view.state = ArtifactState::Ready;
    (StatusCode::OK, Json(artifact.view.clone())).into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct CompleteUploadRequest {
    pub(super) artifact_id: String,
    pub(super) size_bytes:  u64,
    pub(super) sha256:      String,
}

#[derive(Debug, Serialize)]
pub(super) struct DownloadResponse {
    pub(super) artifact: ArtifactView,
    pub(super) download: nagisalake_protocol::PresignedRequest,
}

pub(super) async fn download_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(artifact_id): AxumPath<String>,
) -> Response {
    if let Err(error) = require_consumer(&headers, &state.config) {
        return api_error(error);
    }
    let organization_id = state.config.auth.legacy_organization_id.clone();
    // Same store fallback as the /api/v1 path: ready artifacts and completed
    // jobs are no longer held in memory.
    let artifact = match artifact_record(&state, &organization_id, &artifact_id).await {
        Ok(artifact) => artifact,
        Err(error) => return api_error(error),
    };
    if artifact.view.state != ArtifactState::Ready {
        return api_error(HubError::Conflict("artifact is not ready".into()));
    }
    if let Some(job_id) = &artifact.view.job_id {
        match job_state_for(&state, &organization_id, job_id).await {
            Ok(JobState::Completed) => {}
            Ok(_) => {
                return api_error(HubError::Conflict("output is not available yet".into()));
            }
            Err(error) => return api_error(error),
        }
    }
    let download = match state.objects.presign_get(&artifact.object_key).await {
        Ok(download) => download,
        Err(error) => return api_error(HubError::ObjectStore(error.to_string())),
    };
    Json(DownloadResponse {
        artifact: artifact.view,
        download,
    })
    .into_response()
}

pub(super) async fn submit_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SubmitJobRequest>,
) -> Response {
    if let Err(error) = require_consumer(&headers, &state.config) {
        return api_error(error);
    }
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if !request.parameters.is_object() {
        return api_error(HubError::InvalidRequest(
            "parameters must be a JSON object".into(),
        ));
    }
    if request.workflow_id.trim().is_empty() || request.workflow_version.trim().is_empty() {
        return api_error(HubError::InvalidRequest(
            "workflow_id and workflow_version are required".into(),
        ));
    }
    let request_hash =
        hash_secret(&serde_json::to_string(&request).unwrap_or_else(|_| "invalid-request".into()));
    let idempotency_map_key = key.as_ref().map(|key| {
        format!(
            "{}\0legacy_token\0legacy_consumer\0/v1/jobs\0{}",
            state.config.auth.legacy_organization_id, key
        )
    });
    if let (Some(store), Some(key)) = (state.store.as_ref(), key.as_deref()) {
        match store
            .idempotency(
                &state.config.auth.legacy_organization_id,
                "legacy_token",
                "legacy_consumer",
                "/v1/jobs",
                key,
            )
            .await
        {
            Ok(Some(existing)) if existing.request_hash != request_hash => {
                return api_error(HubError::Conflict(
                    "idempotency key was already used for a different request".into(),
                ));
            }
            Ok(Some(existing)) => {
                return match state
                    .data
                    .read()
                    .await
                    .jobs
                    .get(&existing.job_id)
                    .map(|job| job.view.clone())
                {
                    Some(view) => (StatusCode::ACCEPTED, Json(view)).into_response(),
                    None => api_error(HubError::Conflict("persisted job is not loaded".into())),
                };
            }
            Ok(None) => {}
            Err(error) => return api_error(HubError::Store(error)),
        }
    }
    if let Some(key) = &idempotency_map_key {
        let cached_view = {
            let data = state.data.read().await;
            data.idempotency
                .get(key)
                .map(|job_id| data.jobs.get(job_id).map(|job| job.view.clone()))
        };
        if let Some(cached_view) = cached_view {
            return match cached_view {
                Some(view) => (StatusCode::ACCEPTED, Json(view)).into_response(),
                None => api_error(HubError::Conflict("idempotency record is stale".into())),
            };
        }
    }
    let input_records = {
        let data = state.data.read().await;
        let mut records = Vec::with_capacity(request.input_artifact_ids.len());
        for artifact_id in &request.input_artifact_ids {
            let Some(artifact) = data.artifacts.get(artifact_id) else {
                return api_error(HubError::NotFound(format!("input artifact {artifact_id}")));
            };
            if artifact.organization_id != state.config.auth.legacy_organization_id {
                return api_error(HubError::NotFound(format!("input artifact {artifact_id}")));
            }
            if artifact.view.state != ArtifactState::Ready || artifact.view.job_id.is_some() {
                return api_error(HubError::Conflict(format!(
                    "input artifact {artifact_id} is not ready"
                )));
            }
            records.push(artifact.clone());
        }
        records
    };
    let mut inputs = Vec::with_capacity(input_records.len());
    for artifact in &input_records {
        let download = match state.objects.presign_get(&artifact.object_key).await {
            Ok(download) => download,
            Err(error) => return api_error(HubError::ObjectStore(error.to_string())),
        };
        inputs.push(JobInput {
            artifact_id: artifact.view.id.clone(),
            name: artifact.view.name.clone(),
            content_type: artifact.view.content_type.clone(),
            size_bytes: artifact.view.size_bytes,
            sha256: artifact.view.sha256.clone(),
            download,
        });
    }
    let job_id = Uuid::new_v4().to_string();
    let command_id = Uuid::new_v4().to_string();
    let worker = state
        .sessions
        .reserve_capacity(&command_id, |worker| {
            worker.organization_id == state.config.auth.legacy_organization_id
                && worker.capabilities.workflows.iter().any(|workflow| {
                    workflow.id == request.workflow_id
                        && workflow.version == request.workflow_version
                })
        })
        .await;
    let Some(worker) = worker else {
        return api_error(HubError::Unavailable(
            "no connected worker has the requested workflow and capacity".into(),
        ));
    };
    let dispatch = DispatchJob {
        command_id: command_id.clone(),
        job_id: job_id.clone(),
        attempt: 1,
        workflow_id: request.workflow_id.clone(),
        workflow_version: request.workflow_version.clone(),
        parameters: request.parameters.clone(),
        inputs,
    };
    let command_id = dispatch.command_id.clone();
    let now = now_unix_ms();
    let view = JobView {
        id:                  job_id.clone(),
        workflow_id:         request.workflow_id,
        workflow_version:    request.workflow_version,
        parameters:          request.parameters,
        input_artifact_ids:  request.input_artifact_ids.clone(),
        output_artifact_ids: Vec::new(),
        worker_id:           worker.worker_id.to_string(),
        session_id:          worker.session_id.to_string(),
        state:               JobState::Received,
        progress:            None,
        prompt_id:           None,
        error:               None,
        events:              Vec::new(),
        created_at_unix_ms:  now,
        updated_at_unix_ms:  now,
    };
    let record = JobRecord {
        organization_id:        state.config.auth.legacy_organization_id.clone(),
        actor_id:               "legacy_consumer".into(),
        actor_kind:             "legacy_token".into(),
        actor_user_id:          None,
        worker_organization_id: state.config.auth.legacy_organization_id.clone(),
        view:                   view.clone(),
        dispatch:               dispatch.clone(),
        last_event:             0,
    };
    match commit_job_record(
        &state,
        &record,
        "/v1/jobs",
        key.as_deref(),
        &request_hash,
        None,
    )
    .await
    {
        Ok(CommitJobResult::Created) => {}
        Ok(CommitJobResult::Existing { job_id }) => {
            state
                .sessions
                .release_capacity_reservation(
                    &worker.organization_id,
                    &worker.worker_id,
                    &worker.session_id,
                    &command_id,
                )
                .await;
            return state
                .data
                .read()
                .await
                .jobs
                .get(&job_id)
                .map(|job| (StatusCode::ACCEPTED, Json(job.view.clone())).into_response())
                .unwrap_or_else(|| {
                    api_error(HubError::Conflict("persisted job is not loaded".into()))
                });
        }
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
            return api_error(error);
        }
    }
    {
        let mut data = state.data.write().await;
        if let Some(key) = &idempotency_map_key {
            data.idempotency.insert(key.clone(), job_id.clone());
        }
        for artifact in input_records {
            if let Some(stored) = data.artifacts.get_mut(&artifact.view.id) {
                stored.view.job_id = Some(job_id.clone());
            }
        }
        data.jobs.insert(job_id.clone(), record.clone());
    }
    let command_id = dispatch.command_id.clone();
    match state
        .sessions
        .send_command(
            &state.config.auth.legacy_organization_id,
            &worker.worker_id,
            &worker.session_id,
            &command_id,
            HubMessage::DispatchJob(dispatch),
            Duration::from_secs(state.config.transport.command_ack_timeout_seconds),
        )
        .await
    {
        Ok(ack) if ack.accepted => {
            if let Some(store) = state.store.as_ref() {
                let _ = store
                    .mark_dispatch_delivered(&record.organization_id, &job_id, 1)
                    .await;
            }
            (StatusCode::ACCEPTED, Json(view)).into_response()
        }
        Ok(ack) => {
            mark_job_failed(&state, &job_id, ack.message).await;
            api_error(HubError::Conflict("worker rejected the job".into()))
        }
        Err(error) => {
            record_dispatch_error(&state, &job_id, error.to_string()).await;
            if let Some(store) = state.store.as_ref() {
                let _ = store
                    .record_dispatch_error(&record.organization_id, &job_id, 1, &error.to_string())
                    .await;
            }
            api_error(error)
        }
    }
}

pub(crate) async fn accessible_workers_for(
    state: &AppState,
    principal: &Principal,
) -> Result<Vec<WorkerView>, HubError> {
    let devices = device_access_for_principal(state, principal).await?;
    // Grouped by organization so the membership test below borrows both ids.
    // Keying a flat set on `session_key` cost one formatted allocation per
    // device to build and another per connected worker to probe.
    let mut allowed: HashMap<String, HashMap<String, nagisalake_hub_store::DeviceAccess>> =
        HashMap::new();
    for device in devices {
        allowed
            .entry(device.device_organization_id.clone())
            .or_default()
            .insert(device.device_id.clone(), device);
    }
    // Filter first: cloning a view copies its capabilities, manifests and
    // labels, and this ran over every session on the Hub before narrowing to
    // the caller's own devices.
    Ok(state
        .sessions
        .list_matching_workflows(
            |worker| {
                allowed
                    .get(worker.organization_id.as_str())
                    .and_then(|devices| devices.get(worker.worker_id.as_str()))
                    .is_some()
            },
            |worker, workflow| {
                allowed
                    .get(worker.organization_id.as_str())
                    .and_then(|devices| devices.get(worker.worker_id.as_str()))
                    .is_some_and(|access| {
                        device_workflow_allowed(
                            &access.allowed_workflows,
                            &workflow.id,
                            &workflow.version,
                        )
                    })
            },
        )
        .await)
}

pub(crate) async fn device_access_for_principal(
    state: &AppState,
    principal: &Principal,
) -> Result<Vec<nagisalake_hub_store::DeviceAccess>, HubError> {
    let user_id = principal
        .user_id
        .as_deref()
        .ok_or_else(|| HubError::Forbidden("device access requires a user identity".into()))?;
    let store = state.store.as_ref().ok_or_else(|| {
        HubError::Unavailable("PostgreSQL control plane is not configured".into())
    })?;
    if let Some(devices) = state
        .cached_device_access(&principal.organization_id, user_id)
        .await
    {
        return Ok(devices);
    }
    let devices = store
        .device_access_for_user(user_id, &principal.organization_id)
        .await?;
    state
        .cache_device_access(&principal.organization_id, user_id, devices.clone())
        .await;
    Ok(devices)
}

pub(crate) async fn create_upload_for_principal(
    state: &AppState,
    principal: &Principal,
    request: CreateUploadRequest,
) -> Result<CreateUploadResponse, HubError> {
    validate_artifact_metadata(
        &request.name,
        &request.content_type,
        request.size_bytes,
        &request.sha256,
        state.config.transport.max_artifact_bytes,
    )?;
    if state.store.is_none() {
        return Err(HubError::Unavailable(
            "PostgreSQL control plane is not configured".into(),
        ));
    }
    state
        .reserve_storage(
            &principal.organization_id,
            request.size_bytes.min(i64::MAX as u64) as i64,
        )
        .await
        .map_err(map_store_error)?;
    let artifact_id = Uuid::new_v4().to_string();
    let object_key = format!(
        "organizations/{}/inputs/{artifact_id}/{}",
        principal.organization_id,
        safe_filename(&request.name)
    );
    let upload = match state
        .objects
        .presign_put(
            &object_key,
            &request.content_type,
            request.size_bytes,
            &request.sha256,
        )
        .await
    {
        Ok(upload) => upload,
        Err(error) => {
            let _ = state
                .release_storage(
                    &principal.organization_id,
                    request.size_bytes.min(i64::MAX as u64) as i64,
                )
                .await;
            return Err(HubError::ObjectStore(error.to_string()));
        }
    };
    let view = ArtifactView {
        id:           artifact_id.clone(),
        job_id:       None,
        name:         request.name,
        content_type: request.content_type,
        size_bytes:   request.size_bytes,
        sha256:       request.sha256.to_ascii_lowercase(),
        state:        ArtifactState::PendingUpload,
    };
    let record = ArtifactRecord {
        organization_id: principal.organization_id.clone(),
        view: view.clone(),
        object_key,
    };
    if let Err(error) = persist_artifact(state, &record).await {
        let _ = state
            .release_storage(
                &principal.organization_id,
                request.size_bytes.min(i64::MAX as u64) as i64,
            )
            .await;
        return Err(error);
    }
    state
        .data
        .write()
        .await
        .artifacts
        .insert(artifact_id, record);
    Ok(CreateUploadResponse {
        artifact: view,
        upload,
    })
}

pub(crate) async fn complete_upload_for_principal(
    state: &AppState,
    principal: &Principal,
    artifact_id: &str,
    request: CompleteUploadRequest,
) -> Result<ArtifactView, HubError> {
    if artifact_id != request.artifact_id {
        return Err(HubError::InvalidRequest(
            "artifact id does not match path".into(),
        ));
    }
    let artifact = state
        .data
        .read()
        .await
        .artifacts
        .get(artifact_id)
        .filter(|artifact| artifact.organization_id == principal.organization_id)
        .cloned()
        .ok_or_else(|| HubError::NotFound("artifact".into()))?;
    if artifact.view.state != ArtifactState::PendingUpload {
        return Ok(artifact.view);
    }
    let metadata = state
        .objects
        .head(&artifact.object_key)
        .await
        .map_err(|error| HubError::ObjectStore(error.to_string()))?;
    if metadata.size_bytes != request.size_bytes
        || request.size_bytes != artifact.view.size_bytes
        || !request.sha256.eq_ignore_ascii_case(&artifact.view.sha256)
        || !object_metadata_matches(&metadata, &artifact.view)
    {
        // The client uploaded something that does not match the declaration.
        // The presigned URL bound content-type and the size bound was checked at
        // reservation, but a malicious or buggy client can still land a stray
        // object. Drop it and release the reserved storage so neither the
        // bucket nor the quota leak.
        let _ = state.objects.delete(&artifact.object_key).await;
        let _ = state
            .release_storage(
                &principal.organization_id,
                artifact.view.size_bytes.min(i64::MAX as u64) as i64,
            )
            .await;
        state.data.write().await.artifacts.remove(artifact_id);
        return Err(HubError::Conflict(
            "object size, content type, or SHA-256 does not match the upload declaration".into(),
        ));
    }
    let mut completed = artifact;
    completed.view.state = ArtifactState::Ready;
    persist_artifact(state, &completed).await?;
    state
        .invalidate_cached_artifact(&completed.organization_id, artifact_id)
        .await;
    state
        .data
        .write()
        .await
        .artifacts
        .insert(artifact_id.into(), completed.clone());
    Ok(completed.view)
}

/// Loads an artifact, falling back to the store for ones no longer resident.
///
/// Only `pending_upload` artifacts stay in memory, so every completed output is
/// served from PostgreSQL.
pub(super) async fn artifact_record(
    state: &AppState,
    organization_id: &str,
    artifact_id: &str,
) -> Result<ArtifactRecord, HubError> {
    let cached = state
        .data
        .read()
        .await
        .artifacts
        .get(artifact_id)
        .filter(|artifact| artifact.organization_id == organization_id)
        .cloned();
    if let Some(artifact) = cached {
        return Ok(artifact);
    }
    if let Some(artifact) = state.cached_artifact(organization_id, artifact_id).await {
        return Ok(artifact);
    }
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| HubError::NotFound("artifact".into()))?;
    let stored = store
        .artifact(organization_id, artifact_id)
        .await
        .map_err(HubError::Store)?
        .ok_or_else(|| HubError::NotFound("artifact".into()))?;
    let size_bytes = u64::try_from(stored.size_bytes)
        .map_err(|_| HubError::InvalidConfig("persisted artifact size is negative".into()))?;
    let record = ArtifactRecord {
        organization_id: stored.organization_id,
        view:            ArtifactView {
            id: stored.id,
            job_id: stored.job_id,
            name: stored.name,
            content_type: stored.content_type,
            size_bytes,
            sha256: stored.sha256,
            state: parse_artifact_state(&stored.state)?,
        },
        object_key:      stored.object_key,
    };
    if record.view.state == ArtifactState::Ready {
        state
            .cache_artifact(organization_id, artifact_id, record.clone())
            .await;
    }
    Ok(record)
}

/// Reads a job's state, from memory when resident and from the store otherwise.
pub(super) async fn job_state_for(
    state: &AppState,
    organization_id: &str,
    job_id: &str,
) -> Result<JobState, HubError> {
    if let Some(job) = state
        .data
        .read()
        .await
        .jobs
        .get(job_id)
        .filter(|job| job.organization_id == organization_id)
    {
        return Ok(job.view.state);
    }
    if let Some(job) = state.cached_job(organization_id, job_id).await {
        return Ok(job.state);
    }
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| HubError::NotFound("job".into()))?;
    let stored = store
        .job(organization_id, job_id)
        .await
        .map_err(HubError::Store)?
        .ok_or_else(|| HubError::NotFound("job".into()))?;
    parse_job_state(&stored.state)
}

pub(crate) async fn download_for_principal(
    state: &AppState,
    principal: &Principal,
    artifact_id: &str,
) -> Result<DownloadResponse, HubError> {
    let artifact = readable_artifact_for_principal(state, principal, artifact_id).await?;
    let download = state
        .objects
        .presign_get(&artifact.object_key)
        .await
        .map_err(|error| HubError::ObjectStore(error.to_string()))?;
    Ok(DownloadResponse {
        artifact: artifact.view,
        download,
    })
}

/// Applies the existing tenant/readiness/completed-job policy before a caller
/// receives either a presigned request or a same-origin byte stream.
pub(crate) async fn readable_artifact_for_principal(
    state: &AppState,
    principal: &Principal,
    artifact_id: &str,
) -> Result<ArtifactRecord, HubError> {
    let artifact = artifact_record(state, &principal.organization_id, artifact_id).await?;
    if artifact.view.state != ArtifactState::Ready {
        return Err(HubError::Conflict("artifact is not ready".into()));
    }
    // Outputs are only downloadable once their job completed. The job is
    // terminal by then, so it is no longer resident and comes from the store.
    if let Some(job_id) = &artifact.view.job_id
        && job_state_for(state, &principal.organization_id, job_id).await? != JobState::Completed
    {
        return Err(HubError::Conflict("output is not available yet".into()));
    }
    Ok(artifact)
}

pub(crate) async fn submit_job_for_principal(
    state: &AppState,
    principal: &Principal,
    idempotency_key: Option<&str>,
    request: SubmitJobRequest,
) -> Result<JobView, HubError> {
    if !request.parameters.is_object() {
        return Err(HubError::InvalidRequest(
            "parameters must be a JSON object".into(),
        ));
    }
    if request.workflow_id.trim().is_empty() || request.workflow_version.trim().is_empty() {
        return Err(HubError::InvalidRequest(
            "workflow_id and workflow_version are required".into(),
        ));
    }
    if request.device_id.is_some() != request.device_organization_id.is_some() {
        return Err(HubError::InvalidRequest(
            "device_id and device_organization_id must be provided together".into(),
        ));
    }
    let store = state.store.as_ref().ok_or_else(|| {
        HubError::Unavailable("PostgreSQL control plane is not configured".into())
    })?;
    let actor_id = if principal.kind == PrincipalKind::BrowserSession {
        principal.user_id.as_deref().unwrap_or(&principal.actor_id)
    } else {
        &principal.actor_id
    };
    let actor_kind = principal_kind_name(principal.kind);
    let request_hash =
        hash_secret(&serde_json::to_string(&request).unwrap_or_else(|_| "invalid-request".into()));
    if let Some(key) = idempotency_key
        && let Some(existing) = store
            .idempotency(
                &principal.organization_id,
                actor_kind,
                actor_id,
                "/api/v1/jobs",
                key,
            )
            .await?
    {
        if existing.request_hash != request_hash {
            return Err(HubError::Conflict(
                "idempotency key was already used for a different request".into(),
            ));
        }
        return job_for_principal(state, principal, &existing.job_id).await;
    }
    let workers = accessible_workers_for(state, principal).await?;
    let accessible_workers = workers
        .into_iter()
        .map(|worker| (worker.organization_id, worker.worker_id))
        .collect::<HashSet<_>>();
    let cached_inputs = {
        let data = state.data.read().await;
        request
            .input_artifact_ids
            .iter()
            .map(|artifact_id| {
                data.artifacts
                    .get(artifact_id)
                    .filter(|artifact| artifact.organization_id == principal.organization_id)
                    .cloned()
            })
            .collect::<Vec<_>>()
    };
    // Inputs uploaded before a restart are no longer resident, so fill the gaps
    // from the store rather than rejecting a perfectly valid submission.
    let mut input_records = Vec::with_capacity(cached_inputs.len());
    for (artifact_id, cached) in request.input_artifact_ids.iter().zip(cached_inputs) {
        let artifact = match cached {
            Some(artifact) => artifact,
            None => artifact_record(state, &principal.organization_id, artifact_id)
                .await
                .map_err(|_| HubError::NotFound(format!("input artifact {artifact_id}")))?,
        };
        if artifact.view.state != ArtifactState::Ready || artifact.view.job_id.is_some() {
            return Err(HubError::Conflict(format!(
                "input artifact {artifact_id} is not ready"
            )));
        }
        input_records.push(artifact);
    }
    let mut inputs = Vec::with_capacity(input_records.len());
    for artifact in &input_records {
        let download = state
            .objects
            .presign_get(&artifact.object_key)
            .await
            .map_err(|error| HubError::ObjectStore(error.to_string()))?;
        inputs.push(JobInput {
            artifact_id: artifact.view.id.clone(),
            name: artifact.view.name.clone(),
            content_type: artifact.view.content_type.clone(),
            size_bytes: artifact.view.size_bytes,
            sha256: artifact.view.sha256.clone(),
            download,
        });
    }
    let job_id = Uuid::new_v4().to_string();
    let command_id = Uuid::new_v4().to_string();
    let worker = state
        .sessions
        .reserve_capacity(&command_id, |worker| {
            accessible_workers.contains(&(worker.organization_id.clone(), worker.worker_id.clone()))
                && request
                    .device_organization_id
                    .as_deref()
                    .is_none_or(|organization| organization == worker.organization_id)
                && request
                    .device_id
                    .as_deref()
                    .is_none_or(|device| device == worker.worker_id)
                && worker.capabilities.workflows.iter().any(|workflow| {
                    workflow.id == request.workflow_id
                        && workflow.version == request.workflow_version
                })
        })
        .await
        .ok_or_else(|| {
            HubError::Unavailable(
                "no accessible connected device has the requested workflow and capacity".into(),
            )
        })?;
    let dispatch = DispatchJob {
        command_id: command_id.clone(),
        job_id: job_id.clone(),
        attempt: 1,
        workflow_id: request.workflow_id.clone(),
        workflow_version: request.workflow_version.clone(),
        parameters: request.parameters.clone(),
        inputs,
    };
    let now = now_unix_ms();
    let view = JobView {
        id:                  job_id.clone(),
        workflow_id:         request.workflow_id,
        workflow_version:    request.workflow_version,
        parameters:          request.parameters,
        input_artifact_ids:  request.input_artifact_ids,
        output_artifact_ids: Vec::new(),
        worker_id:           worker.worker_id.to_string(),
        session_id:          worker.session_id.to_string(),
        state:               JobState::Received,
        progress:            None,
        prompt_id:           None,
        error:               None,
        events:              Vec::new(),
        created_at_unix_ms:  now,
        updated_at_unix_ms:  now,
    };
    let record = JobRecord {
        organization_id:        principal.organization_id.clone(),
        actor_id:               actor_id.into(),
        actor_kind:             actor_kind.into(),
        actor_user_id:          principal.user_id.clone(),
        worker_organization_id: worker.organization_id.to_string(),
        view:                   view.clone(),
        dispatch:               dispatch.clone(),
        last_event:             0,
    };
    let user_id = principal
        .user_id
        .as_deref()
        .ok_or_else(|| HubError::Forbidden("device access requires a user identity".into()))?;
    let admission = DeviceUseAdmission {
        organization_id: &record.organization_id,
        user_id,
        device_organization_id: &record.worker_organization_id,
        device_id: &record.view.worker_id,
        workflow_id: &record.view.workflow_id,
        workflow_version: &record.view.workflow_version,
        requested_jobs: 1,
        now,
    };
    let commit = commit_job_record(
        state,
        &record,
        "/api/v1/jobs",
        idempotency_key,
        &request_hash,
        Some(admission),
    )
    .await;
    match commit {
        Ok(CommitJobResult::Created) => {}
        Ok(CommitJobResult::Existing { job_id }) => {
            state
                .sessions
                .release_capacity_reservation(
                    &worker.organization_id,
                    &worker.worker_id,
                    &worker.session_id,
                    &command_id,
                )
                .await;
            return job_for_principal(state, principal, &job_id).await;
        }
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
            return Err(error);
        }
    }
    {
        let mut data = state.data.write().await;
        for artifact in input_records {
            // `commit_new_job` already bound job_id inside its transaction, so a
            // miss here just means the artifact is not cached; the store is
            // authoritative either way.
            if let Some(stored) = data.artifacts.get_mut(&artifact.view.id) {
                stored.view.job_id = Some(job_id.clone());
            }
        }
        data.jobs.insert(job_id.clone(), record.clone());
    }
    let command_id = dispatch.command_id.clone();
    match state
        .sessions
        .send_command(
            &worker.organization_id,
            &worker.worker_id,
            &worker.session_id,
            &command_id,
            HubMessage::DispatchJob(dispatch),
            Duration::from_secs(state.config.transport.command_ack_timeout_seconds),
        )
        .await
    {
        Ok(ack) if ack.accepted => {
            store
                .mark_dispatch_delivered(&record.organization_id, &job_id, 1)
                .await?;
            Ok(view)
        }
        Ok(ack) => {
            mark_job_failed(state, &job_id, ack.message).await;
            Err(HubError::Conflict("worker rejected the job".into()))
        }
        Err(error) => {
            record_dispatch_error(state, &job_id, error.to_string()).await;
            store
                .record_dispatch_error(&record.organization_id, &job_id, 1, &error.to_string())
                .await?;
            Err(error)
        }
    }
}
