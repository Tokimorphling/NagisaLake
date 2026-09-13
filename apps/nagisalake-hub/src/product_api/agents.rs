use super::{authentication::authorize_current, shared::*, *};
use crate::agent::{map_agent_error, owner_id};
use nagisalake_agent::{RunEvent, RunHandle, RunRequest};

pub(super) async fn principal(
    state: &AppState,
    headers: &HeaderMap,
    permission: Permission,
) -> Result<Principal, HubError> {
    if state.store.is_some() {
        return Ok(authorize_current(state, headers, permission)
            .await?
            .principal);
    }
    // Local/legacy deployments have no account database. This path still
    // requires the configured bearer credential; it is never anonymous.
    crate::require_consumer(headers, &state.config)?;
    Ok(Principal {
        kind:            PrincipalKind::LegacyToken,
        actor_id:        "legacy_consumer".into(),
        user_id:         None,
        organization_id: state.config.auth.legacy_organization_id.clone(),
        role:            Role::Owner,
        scopes:          BTreeSet::new(),
    })
}

pub(super) async fn skills(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let id = request_id(&headers);
    if let Err(error) = principal(&state, &headers, Permission::JobsWrite).await {
        return product_error(error, &id);
    }
    Json(json!({"enabled":state.agent.is_some(),"items":state.agent.as_ref().map(|agent| agent.service.skills().cloned().collect::<Vec<_>>()).unwrap_or_default()})).into_response()
}

async fn start(
    state: &AppState,
    headers: &HeaderMap,
    request: RunRequest,
) -> Result<RunHandle, HubError> {
    let principal = principal(state, headers, Permission::JobsWrite).await?;
    let agent = state
        .agent
        .as_ref()
        .ok_or_else(|| HubError::Unavailable("agent integration is not configured".into()))?;
    agent
        .service
        .validate_request(&request)
        .map_err(map_agent_error)?;
    // Do not reuse GPU quota usage counters; the agent has independent global
    // and organization concurrency limits plus the standard submission rate.
    state
        .rate_limit_key(
            "agent_submit",
            &principal.organization_id,
            state.rate_limiter.limits().submit_per_org,
        )
        .await?;
    agent.start(&principal, request, state.store.clone()).await
}

pub(super) async fn run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RunRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let mut run = match start(&state, &headers, request).await {
        Ok(run) => run,
        Err(error) => return product_error(error, &request_id),
    };
    while let Some(event) = run.recv().await {
        match event {
            RunEvent::Completed { .. } => return Json(event).into_response(),
            RunEvent::Error { ref code, .. } => {
                let status = if code == "timeout" {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                };
                return (status, Json(event)).into_response();
            }
            RunEvent::Cancelled { .. } => {
                return (StatusCode::CONFLICT, Json(event)).into_response();
            }
            _ => {}
        }
    }
    product_error(
        HubError::Unavailable("agent execution ended unexpectedly".into()),
        &request_id,
    )
}

pub(super) async fn stream_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RunRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let run = match start(&state, &headers, request).await {
        Ok(run) => run,
        Err(error) => return product_error(error, &request_id),
    };
    let events = stream::unfold((run, 0_u64), |(mut run, sequence)| async move {
        let event = run.recv().await?;
        let sequence = sequence + 1;
        let frame = Event::default()
            .event(event.event_name())
            .id(sequence.to_string())
            .json_data(event)
            .expect("serializable agent event");
        Some((Ok::<_, Infallible>(frame), (run, sequence)))
    });
    // Dropping the HTTP body drops RunHandle and cancels upstream execution.
    let mut response = Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

pub(super) async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let principal = match principal(&state, &headers, Permission::JobsCancelOwn).await {
        Ok(principal) => principal,
        Err(error) => return product_error(error, &request_id),
    };
    if state
        .agent
        .as_ref()
        .is_some_and(|agent| agent.cancel(&principal, &id))
    {
        return StatusCode::NO_CONTENT.into_response();
    }
    if let Some(store) = &state.store {
        match store
            .agent_execution(
                &principal.organization_id,
                owner_id(&principal),
                &id,
                now_unix_ms(),
            )
            .await
        {
            Ok(Some(record)) if record.state != "running" => {
                return StatusCode::NO_CONTENT.into_response();
            }
            Ok(Some(_)) => {
                return product_error(
                    HubError::Conflict("execution is not active on this instance".into()),
                    &request_id,
                );
            }
            Err(error) => return product_error(HubError::Store(error), &request_id),
            _ => {}
        }
    }
    product_error(HubError::NotFound("agent execution".into()), &request_id)
}

pub(super) async fn get_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let principal = match principal(&state, &headers, Permission::JobsReadOrganization).await {
        Ok(principal) => principal,
        Err(error) => return product_error(error, &request_id),
    };
    let Some(store) = &state.store else {
        return product_error(
            HubError::Unavailable("agent history requires PostgreSQL".into()),
            &request_id,
        );
    };
    match store
        .agent_execution(
            &principal.organization_id,
            owner_id(&principal),
            &id,
            now_unix_ms(),
        )
        .await
    {
        Ok(Some(record)) => Json(record).into_response(),
        Ok(None) => product_error(HubError::NotFound("agent execution".into()), &request_id),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}
