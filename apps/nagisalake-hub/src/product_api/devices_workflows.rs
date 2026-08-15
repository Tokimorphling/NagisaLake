use super::{
    authentication::{authorize_current, require_browser},
    shared::*,
    *,
};

pub(super) async fn list_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CursorQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.role.allows(Permission::DevicesRead) {
        return product_error(
            HubError::Forbidden("missing permission devices:read".into()),
            &request_id,
        );
    }
    let user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    let limit = page_limit(query.limit);
    let after = match query.cursor.as_deref() {
        Some(cursor) => match decode_device_cursor(cursor) {
            Ok(value) => Some(value),
            Err(error) => return product_error(error, &request_id),
        },
        None => None,
    };
    let mut devices = match store(&state)
        .unwrap()
        .devices_for_user_page(
            user_id,
            &auth.principal.organization_id,
            limit + 1,
            after
                .as_ref()
                .map(|(org, id, kind)| (org.as_str(), id.as_str(), kind.as_str())),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let has_more = devices.len() > limit as usize;
    if has_more {
        devices.pop();
    }
    let next_cursor = has_more.then(|| devices.last()).flatten().map(|device| {
        encode_device_cursor(
            &device.device_organization_id,
            &device.device_id,
            &device.access_kind,
        )
    });
    let connected = state.sessions.connected_identities().await;
    let devices = devices
        .into_iter()
        .map(|device| {
            let is_connected = connected
                .get(device.device_organization_id.as_str())
                .is_some_and(|workers| workers.contains(device.device_id.as_str()));
            public_device(device, is_connected)
        })
        .collect::<Vec<_>>();
    Json(ListPage {
        items: devices,
        next_cursor,
    })
    .into_response()
}

#[derive(Debug, Serialize)]
pub(super) struct PublicDevice {
    device_organization_id: String,
    device_id:              String,
    owner_user_id:          Option<String>,
    namespace:              String,
    node_name:              String,
    worker_version:         String,
    access_kind:            String,
    connected:              bool,
    workflows:              Vec<PublicDeviceWorkflow>,
    allowed_workflows:      Vec<DeviceWorkflowRule>,
    max_concurrent_jobs:    Option<i64>,
    grant_expires_at:       Option<i64>,
}

#[derive(Debug, Serialize)]
pub(super) struct PublicDeviceWorkflow {
    id:           String,
    version:      String,
    output_types: Vec<String>,
}

fn public_device(device: nagisalake_hub_store::DeviceView, connected: bool) -> PublicDevice {
    let allowed_workflows = device.allowed_workflows;
    let workflows = serde_json::from_str::<JsonValue>(&device.capabilities_json)
        .ok()
        .and_then(|value| value.get("workflows").cloned())
        .and_then(|value| value.as_array().cloned())
        .map(|workflows| {
            workflows
                .into_iter()
                .filter_map(|workflow| {
                    let id = workflow.get("id")?.as_str()?.to_owned();
                    let version = workflow.get("version")?.as_str()?.to_owned();
                    if !device_workflow_allowed(&allowed_workflows, &id, &version) {
                        return None;
                    }
                    Some(PublicDeviceWorkflow {
                        id,
                        version,
                        output_types: workflow
                            .get("output_types")
                            .and_then(JsonValue::as_array)
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(JsonValue::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    PublicDevice {
        device_organization_id: device.device_organization_id,
        device_id: device.device_id,
        owner_user_id: device.owner_user_id,
        namespace: device.namespace,
        node_name: device.node_name,
        worker_version: device.worker_version,
        access_kind: device.access_kind,
        connected,
        workflows,
        allowed_workflows,
        max_concurrent_jobs: device.max_concurrent_jobs,
        grant_expires_at: device.grant_expires_at,
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateDeviceInviteRequest {
    device_organization_id: String,
    device_id:              String,
    #[serde(default = "one_use")]
    max_uses:               i64,
    #[serde(default)]
    expires_in_seconds:     Option<i64>,
    #[serde(default)]
    allowed_workflows:      Vec<DeviceWorkflowRule>,
    #[serde(default)]
    max_concurrent_jobs:    Option<i64>,
    #[serde(default)]
    grant_duration_seconds: Option<i64>,
}
const fn one_use() -> i64 {
    1
}
#[derive(Debug, Serialize)]
pub(super) struct CreatedDeviceInvite {
    invite_id:              String,
    code:                   String,
    code_prefix:            String,
    expires_at:             Option<i64>,
    max_uses:               i64,
    allowed_workflows:      Vec<DeviceWorkflowRule>,
    max_concurrent_jobs:    Option<i64>,
    grant_duration_seconds: Option<i64>,
}
pub(super) async fn create_device_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateDeviceInviteRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&request.device_organization_id)).await
    {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.allows(Permission::DevicesShareOwn) {
        return product_error(
            HubError::Forbidden("role cannot share devices".into()),
            &request_id,
        );
    }
    let owner = auth.principal.user_id.as_deref().unwrap_or_default();
    if request.max_uses < 1 || request.max_uses > 100 {
        return product_error(
            HubError::InvalidRequest("max_uses must be between 1 and 100".into()),
            &request_id,
        );
    }
    if request
        .max_concurrent_jobs
        .is_some_and(|value| !(1..=1_000).contains(&value))
    {
        return product_error(
            HubError::InvalidRequest("max_concurrent_jobs must be between 1 and 1000".into()),
            &request_id,
        );
    }
    if request
        .grant_duration_seconds
        .is_some_and(|value| !(60..=2_592_000).contains(&value))
    {
        return product_error(
            HubError::InvalidRequest(
                "grant_duration_seconds must be between 60 and 2592000".into(),
            ),
            &request_id,
        );
    }
    let code = generate_secret("ndi");
    let id = Uuid::new_v4().to_string();
    let now = now_unix_ms();
    let expires_at = request
        .expires_in_seconds
        .map(|seconds| now + seconds.clamp(60, 2_592_000) * 1_000);
    let store = store(&state).unwrap();
    let device = match store
        .shareable_device_for_user(&request.device_organization_id, &request.device_id, owner)
        .await
    {
        Ok(Some(device)) => device,
        Ok(None) => {
            return product_error(HubError::NotFound("shareable device".into()), &request_id);
        }
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let offered =
        serde_json::from_str::<nagisalake_protocol::WorkerCapabilities>(&device.capabilities_json)
            .map(|capabilities| {
                capabilities
                    .workflows
                    .into_iter()
                    .map(|workflow| (workflow.id, workflow.version))
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
    let mut normalized = BTreeSet::<(String, String)>::new();
    for workflow in request.allowed_workflows {
        if workflow.id.trim().is_empty() || workflow.version.trim().is_empty() {
            return product_error(
                HubError::InvalidRequest("allowed workflow id and version are required".into()),
                &request_id,
            );
        }
        if !offered.contains(&(workflow.id.clone(), workflow.version.clone())) {
            return product_error(
                HubError::InvalidRequest(format!(
                    "device does not offer workflow {}@{}",
                    workflow.id, workflow.version
                )),
                &request_id,
            );
        }
        normalized.insert((workflow.id, workflow.version));
    }
    let allowed_workflows = normalized
        .into_iter()
        .map(|(id, version)| DeviceWorkflowRule { id, version })
        .collect::<Vec<_>>();
    let allowed_workflows_json =
        serde_json::to_string(&allowed_workflows).unwrap_or_else(|_| "[]".into());
    if let Err(error) = store
        .create_device_invite(NewDeviceInvite {
            id: &id,
            organization_id: &request.device_organization_id,
            device_id: &request.device_id,
            owner_user_id: owner,
            code_prefix: &code.display_prefix,
            code_hash: &code.hash,
            max_uses: request.max_uses,
            expires_at,
            created_at: now,
            allowed_workflows_json: &allowed_workflows_json,
            max_concurrent_jobs: request.max_concurrent_jobs,
            grant_duration_seconds: request.grant_duration_seconds,
        })
        .await
    {
        return product_error(map_store(error), &request_id);
    }
    audit(
        &state,
        Some(&request.device_organization_id),
        Some(owner),
        "browser_session",
        &request_id,
        "device_invite.create",
        "device",
        Some(&request.device_id),
        "success",
        json!({
            "invite_id": id,
            "max_uses": request.max_uses,
            "expires_at": expires_at,
            "allowed_workflows": allowed_workflows.clone(),
            "max_concurrent_jobs": request.max_concurrent_jobs,
            "grant_duration_seconds": request.grant_duration_seconds
        }),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(CreatedDeviceInvite {
            invite_id: id,
            code: code.plaintext,
            code_prefix: code.display_prefix,
            expires_at,
            max_uses: request.max_uses,
            allowed_workflows,
            max_concurrent_jobs: request.max_concurrent_jobs,
            grant_duration_seconds: request.grant_duration_seconds,
        }),
    )
        .into_response()
}

pub(super) async fn revoke_device_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(invite_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    let invite = match store.device_invite(&invite_id).await {
        Ok(Some(value)) => value,
        Ok(None) => {
            return product_error(HubError::NotFound("device invite".into()), &request_id);
        }
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let auth = match require_browser(&state, &headers, Some(&invite.organization_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let owner = auth.principal.user_id.as_deref().unwrap_or_default();
    if !auth.principal.allows(Permission::DevicesShareOwn) || invite.owner_user_id != owner {
        return product_error(
            HubError::Forbidden("role cannot revoke this device invite".into()),
            &request_id,
        );
    }
    match store.revoke_device_invite(&invite_id, owner).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => product_error(HubError::NotFound("device invite".into()), &request_id),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct AcceptDeviceInviteRequest {
    code: String,
}
pub(super) async fn accept_device_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AcceptDeviceInviteRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.allows(Permission::DevicesUse) {
        return product_error(
            HubError::Forbidden("missing permission devices:use".into()),
            &request_id,
        );
    }
    let grantee = auth.principal.user_id.as_deref().unwrap_or_default();
    let code_hash = hash_secret(request.code.trim());
    match store(&state)
        .unwrap()
        .accept_device_invite(&code_hash, grantee)
        .await
    {
        Ok(grant) => {
            state
                .invalidate_cached_device_access_for_user(&grant.grantee_user_id)
                .await;
            audit(
                &state,
                Some(&grant.device_organization_id),
                Some(grantee),
                "browser_session",
                &request_id,
                "device_invite.accept",
                "device",
                Some(&grant.device_id),
                "success",
                json!({"grant_id":grant.id}),
            )
            .await;
            (StatusCode::CREATED, Json(grant)).into_response()
        }
        Err(error) => product_error(map_store(error), &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct RevokeDeviceShareRequest {
    device_organization_id: String,
    device_id:              String,
    grantee_user_id:        String,
}
pub(super) async fn revoke_device_share(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RevokeDeviceShareRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&request.device_organization_id)).await
    {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let owner = auth.principal.user_id.as_deref().unwrap_or_default();
    if !auth.principal.allows(Permission::DevicesShareOwn) {
        return product_error(
            HubError::Forbidden("role cannot revoke device shares".into()),
            &request_id,
        );
    }
    match store(&state)
        .unwrap()
        .revoke_device_grant(
            &request.device_organization_id,
            &request.device_id,
            owner,
            &request.grantee_user_id,
        )
        .await
    {
        Ok(true) => {
            state
                .invalidate_cached_device_access_for_user(&request.grantee_user_id)
                .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => product_error(HubError::NotFound("device share".into()), &request_id),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

pub(super) async fn list_workflows(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CursorQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::WorkflowsRead).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let limit = page_limit(query.limit);
    let after = match query.cursor.as_deref() {
        Some(cursor) => match decode_workflow_cursor(cursor) {
            Ok(value) => Some(value),
            Err(error) => return product_error(error, &request_id),
        },
        None => None,
    };
    let mut workers = match accessible_workers_for(&state, &auth.principal).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let user_id = match auth.principal.user_id.as_deref() {
        Some(value) => value,
        None => {
            return product_error(
                HubError::Forbidden("workflow catalog requires a user identity".into()),
                &request_id,
            );
        }
    };
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    let mut persisted = match store
        .workflows_for_user_devices_page(
            user_id,
            &auth.principal.organization_id,
            limit + 1,
            after
                .as_ref()
                .map(|(workflow_id, version)| (workflow_id.as_str(), version.as_str())),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let has_more = persisted.len() > limit as usize;
    if has_more {
        persisted.pop();
    }
    let next_cursor = has_more
        .then(|| persisted.last())
        .flatten()
        .map(|workflow| encode_workflow_cursor(&workflow.workflow_id, &workflow.version));
    // Versions grouped by workflow id so the retain below borrows both halves.
    // A flat set of pairs forced a clone of each id *and* each version for
    // every workflow of every worker, purely to build a throwaway lookup key.
    let mut approved: HashMap<&str, HashSet<&str>> = HashMap::new();
    for workflow in &persisted {
        approved
            .entry(workflow.workflow_id.as_str())
            .or_default()
            .insert(workflow.version.as_str());
    }
    for worker in &mut workers {
        worker.capabilities.workflows.retain(|workflow| {
            approved
                .get(workflow.id.as_str())
                .is_some_and(|versions| versions.contains(workflow.version.as_str()))
        });
    }
    let mut workflows = serde_json::to_value(aggregate_workflows(workers))
        .unwrap_or_else(|_| JsonValue::Array(Vec::new()));
    if let Some(items) = workflows.as_array_mut() {
        for workflow in persisted {
            let exists = items.iter().any(|item| {
                item["id"] == workflow.workflow_id && item["version"] == workflow.version
            });
            if exists {
                continue;
            }
            let manifest = workflow
                .manifest_json
                .as_deref()
                .and_then(|value| serde_json::from_str(value).ok())
                .unwrap_or(JsonValue::Null);
            let output_types = serde_json::from_str::<JsonValue>(&workflow.output_types_json)
                .unwrap_or_else(|_| JsonValue::Array(Vec::new()));
            items.push(json!({
                "id": workflow.workflow_id,
                "version": workflow.version,
                "output_types": output_types,
                "manifest": manifest,
                "manifest_consistent": true,
                "workers": [],
                "available": false
            }));
        }
        items.sort_by(|left, right| {
            left["id"]
                .as_str()
                .unwrap_or_default()
                .cmp(right["id"].as_str().unwrap_or_default())
                .then_with(|| {
                    left["version"]
                        .as_str()
                        .unwrap_or_default()
                        .cmp(right["version"].as_str().unwrap_or_default())
                })
        });
    }
    sanitize_workflow_catalog(&mut workflows);
    let items = workflows.as_array().cloned().unwrap_or_default();
    Json(ListPage { items, next_cursor }).into_response()
}

fn sanitize_workflow_catalog(value: &mut JsonValue) {
    let Some(workflows) = value.as_array_mut() else {
        return;
    };
    for workflow in workflows {
        let Some(workflow) = workflow.as_object_mut() else {
            continue;
        };
        let available = workflow
            .get("workers")
            .and_then(JsonValue::as_array)
            .is_some_and(|workers| workers.iter().any(|worker| worker["available"] == true));
        workflow.insert("available".into(), available.into());
        if let Some(workers) = workflow
            .get_mut("workers")
            .and_then(JsonValue::as_array_mut)
        {
            for worker in workers {
                if let Some(worker) = worker.as_object_mut() {
                    worker.remove("session_id");
                    worker.remove("labels");
                }
            }
        }
        if let Some(inputs) = workflow
            .get_mut("manifest")
            .and_then(JsonValue::as_object_mut)
            .and_then(|manifest| manifest.get_mut("inputs"))
            .and_then(JsonValue::as_array_mut)
        {
            for input in inputs {
                if let Some(input) = input.as_object_mut() {
                    input.remove("pointer");
                    input.remove("node_id");
                    input.remove("node_type");
                    input.remove("field");
                }
            }
        }
    }
}
