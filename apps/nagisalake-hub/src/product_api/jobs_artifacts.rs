use super::{authentication::authorize_current, shared::*, *};

pub(super) async fn create_artifact_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateUploadRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::ArtifactsWrite).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    match create_upload_for_principal(&state, &auth.principal, request).await {
        Ok(response) => {
            audit(
                &state,
                Some(&auth.principal.organization_id),
                auth.principal.user_id.as_deref(),
                auth_kind(auth.principal.kind),
                &request_id,
                "artifact.upload.create",
                "artifact",
                Some(&response.artifact.id),
                "success",
                json!({"size_bytes": response.artifact.size_bytes}),
            )
            .await;
            (StatusCode::CREATED, Json(response)).into_response()
        }
        Err(error) => product_error(error, &request_id),
    }
}

pub(super) async fn complete_artifact_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(artifact_id): Path<String>,
    Json(request): Json<CompleteUploadRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::ArtifactsWrite).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    match complete_upload_for_principal(&state, &auth.principal, &artifact_id, request).await {
        Ok(artifact) => {
            audit(
                &state,
                Some(&auth.principal.organization_id),
                auth.principal.user_id.as_deref(),
                auth_kind(auth.principal.kind),
                &request_id,
                "artifact.upload.complete",
                "artifact",
                Some(&artifact_id),
                "success",
                json!({"size_bytes": artifact.size_bytes}),
            )
            .await;
            Json(artifact).into_response()
        }
        Err(error) => product_error(error, &request_id),
    }
}

pub(super) async fn download_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(artifact_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::ArtifactsRead).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    match download_for_principal(&state, &auth.principal, &artifact_id).await {
        Ok(response) => ([("cache-control", "no-store")], Json(response)).into_response(),
        Err(error) => product_error(error, &request_id),
    }
}

pub(super) async fn submit_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SubmitJobRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsWrite).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.allows(Permission::DevicesUse) {
        return product_error(
            HubError::Forbidden("missing permission devices:use".into()),
            &request_id,
        );
    }
    if let Err(error) = state
        .rate_limit_key(
            "jobs.submit.organization",
            &auth.principal.organization_id,
            state.rate_limiter.limits().submit_per_org,
        )
        .await
    {
        return product_error(error, &request_id);
    }
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match submit_job_for_principal(&state, &auth.principal, idempotency_key, request).await {
        Ok(job) => {
            audit(
                &state,
                Some(&auth.principal.organization_id),
                auth.principal.user_id.as_deref(),
                auth_kind(auth.principal.kind),
                &request_id,
                "job.submit",
                "job",
                Some(&job.id),
                "success",
                json!({"workflow_id": job.workflow_id, "workflow_version": job.workflow_version, "worker_id": job.worker_id}),
            )
            .await;
            (StatusCode::ACCEPTED, Json(job)).into_response()
        }
        Err(error) => product_error(error, &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct JobsQuery {
    #[serde(default)]
    limit:  Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
}

/// Lists jobs newest first, one bounded page at a time.
///
/// `next_cursor` is now a real value: pass it back as `cursor` for the next
/// page, and treat `null` as the end. Rows carry no event timeline — fetch the
/// individual job for that.
pub(super) async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<JobsQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsReadOrganization).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    match jobs_page_for_principal(
        &state,
        &auth.principal,
        query.limit,
        query.cursor.as_deref(),
    )
    .await
    {
        Ok((items, next_cursor)) => {
            Json(json!({"items": items, "next_cursor": next_cursor})).into_response()
        }
        Err(error) => product_error(error, &request_id),
    }
}

pub(super) async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsReadOrganization).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    match job_for_principal(&state, &auth.principal, &job_id).await {
        Ok(job) => Json(job).into_response(),
        Err(error) => product_error(error, &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct JobEventsQuery {
    #[serde(default)]
    after: Option<u64>,
}

/// Streams job event snapshots over the authenticated browser/API request.
/// The client may reconnect with `?after=` using the last event sequence.
pub(super) async fn stream_job_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<JobEventsQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsReadOrganization).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let principal = auth.principal;
    let after = query.after.unwrap_or_default();
    let stream = stream::unfold(
        (state, principal, job_id, after, false),
        |(state, principal, job_id, mut last_sequence, terminal_sent)| async move {
            if terminal_sent {
                return None;
            }
            loop {
                match job_for_principal(&state, &principal, &job_id).await {
                    Ok(view) => {
                        if let Some(event) = view
                            .events
                            .iter()
                            .find(|event| event.sequence > last_sequence)
                        {
                            last_sequence = event.sequence;
                            let is_last_event = view
                                .events
                                .last()
                                .is_some_and(|last| last.sequence == event.sequence);
                            let done = view.state.is_terminal() && is_last_event;
                            let payload = json!({
                                "job_id": view.id,
                                "state": view.state,
                                "progress": view.progress,
                                "error": view.error,
                                "event": event,
                            });
                            let output = Event::default()
                                .id(event.sequence.to_string())
                                .event("job")
                                .data(
                                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()),
                                );
                            return Some((
                                Ok::<Event, Infallible>(output),
                                (state, principal, job_id, last_sequence, done),
                            ));
                        }
                        if view.state.is_terminal() {
                            let payload = json!({
                                "job_id": view.id,
                                "state": view.state,
                                "progress": view.progress,
                                "error": view.error,
                            });
                            let output = Event::default().event("job").data(
                                serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()),
                            );
                            return Some((
                                Ok::<Event, Infallible>(output),
                                (state, principal, job_id, last_sequence, true),
                            ));
                        }
                    }
                    Err(error) => {
                        let output = Event::default().event("error").data(error.to_string());
                        return Some((
                            Ok::<Event, Infallible>(output),
                            (state, principal, job_id, last_sequence, true),
                        ));
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        },
    );
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

pub(super) async fn cancel_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsCancelOwn).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    match cancel_job_for_principal(&state, &auth.principal, &job_id).await {
        Ok(job) => {
            audit(
                &state,
                Some(&auth.principal.organization_id),
                auth.principal.user_id.as_deref(),
                auth_kind(auth.principal.kind),
                &request_id,
                "job.cancel",
                "job",
                Some(&job_id),
                "success",
                json!({}),
            )
            .await;
            Json(job).into_response()
        }
        Err(error) => product_error(error, &request_id),
    }
}
