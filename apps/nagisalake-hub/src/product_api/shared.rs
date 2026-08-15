use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct CursorQuery {
    #[serde(default)]
    pub(super) limit:  Option<i64>,
    #[serde(default)]
    pub(super) cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ListPage<T> {
    pub(super) items:       Vec<T>,
    pub(super) next_cursor: Option<String>,
}

const LIST_PAGE_DEFAULT: i64 = 50;
const LIST_PAGE_MAX: i64 = 200;

pub(super) fn page_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(LIST_PAGE_DEFAULT).clamp(1, LIST_PAGE_MAX)
}

pub(super) fn encode_cursor_parts(parts: &[&str]) -> String {
    data_encoding::BASE64URL_NOPAD.encode(parts.join("\0").as_bytes())
}

pub(super) fn decode_cursor_parts(cursor: &str, expected: usize) -> Result<Vec<String>, HubError> {
    let invalid = || HubError::InvalidRequest("cursor is not valid".into());
    let raw = data_encoding::BASE64URL_NOPAD
        .decode(cursor.as_bytes())
        .map_err(|_| invalid())?;
    let decoded = String::from_utf8(raw).map_err(|_| invalid())?;
    let parts = decoded.split('\0').map(str::to_owned).collect::<Vec<_>>();
    if parts.len() != expected || parts.iter().any(|part| part.is_empty()) {
        return Err(invalid());
    }
    Ok(parts)
}

pub(super) fn encode_created_id_cursor(created_at: i64, id: &str) -> String {
    encode_cursor_parts(&[&created_at.to_string(), id])
}

pub(super) fn decode_created_id_cursor(cursor: &str) -> Result<(i64, String), HubError> {
    let parts = decode_cursor_parts(cursor, 2)?;
    let created_at = parts[0]
        .parse()
        .map_err(|_| HubError::InvalidRequest("cursor is not valid".into()))?;
    Ok((created_at, parts[1].clone()))
}

pub(super) fn encode_id_cursor(id: &str) -> String {
    encode_cursor_parts(&[id])
}

pub(super) fn decode_id_cursor(cursor: &str) -> Result<String, HubError> {
    Ok(decode_cursor_parts(cursor, 1)?[0].clone())
}

pub(super) fn encode_device_cursor(
    organization_id: &str,
    device_id: &str,
    access_kind: &str,
) -> String {
    encode_cursor_parts(&[organization_id, device_id, access_kind])
}

pub(super) fn decode_device_cursor(cursor: &str) -> Result<(String, String, String), HubError> {
    let parts = decode_cursor_parts(cursor, 3)?;
    Ok((parts[0].clone(), parts[1].clone(), parts[2].clone()))
}

pub(super) fn encode_workflow_cursor(workflow_id: &str, version: &str) -> String {
    encode_cursor_parts(&[workflow_id, version])
}

pub(super) fn decode_workflow_cursor(cursor: &str) -> Result<(String, String), HubError> {
    let parts = decode_cursor_parts(cursor, 2)?;
    Ok((parts[0].clone(), parts[1].clone()))
}

pub(super) fn store(state: &AppState) -> Result<&PgStore, HubError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| HubError::Unavailable("PostgreSQL control plane is not configured".into()))
}
pub(super) fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(|value| truncate(value, 128))
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}
pub(super) fn product_error(error: HubError, request_id: &str) -> Response {
    let message = truncate(&error.to_string(), 1_000);
    let (status, code) = match &error {
        HubError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
        HubError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
        HubError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
        HubError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
        HubError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
        HubError::QuotaExceeded(_) => (StatusCode::TOO_MANY_REQUESTS, "quota_exceeded"),
        HubError::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        HubError::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        HubError::ObjectStore(_) | HubError::Transport(_) => {
            (StatusCode::BAD_GATEWAY, "upstream_error")
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    };
    let mut response = (
        status,
        Json(json!({"error":{"code":code,"message":message,"request_id":request_id}})),
    )
        .into_response();
    if let HubError::RateLimited {
        retry_after_seconds,
    } = &error
        && let Ok(value) = HeaderValue::from_str(&retry_after_seconds.to_string())
    {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}
pub(super) fn map_store(error: StoreError) -> HubError {
    match error {
        StoreError::NotFound(value) => HubError::NotFound(value),
        StoreError::Conflict(value) => HubError::Conflict(value),
        StoreError::QuotaExceeded(value) => HubError::QuotaExceeded(value),
        other => HubError::Store(other),
    }
}
pub(super) fn valid_email(email: &str) -> bool {
    let email = email.trim();
    email.len() <= 320
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
}
pub(super) fn auth_kind(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::BrowserSession => "browser_session",
        PrincipalKind::ApiKey => "api_key",
        PrincipalKind::WorkerCredential => "worker_credential",
        PrincipalKind::LegacyToken => "legacy_token",
    }
}
pub(super) fn validate_scopes(
    principal: &Principal,
    requested: &[String],
) -> Result<Vec<String>, HubError> {
    let mut values = BTreeSet::new();
    for scope in requested {
        let permission = scope_permission(scope)
            .ok_or_else(|| HubError::InvalidRequest(format!("unknown API key scope: {scope}")))?;
        if !principal.role.allows(permission) {
            return Err(HubError::Forbidden(format!(
                "role cannot grant scope {scope}"
            )));
        }
        values.insert(scope.clone());
    }
    if values.is_empty() {
        return Err(HubError::InvalidRequest(
            "at least one API key scope is required".into(),
        ));
    }
    Ok(values.into_iter().collect())
}
pub(super) fn scope_permission(scope: &str) -> Option<Permission> {
    Some(match scope {
        "workflows:read" => Permission::WorkflowsRead,
        "workflows:write" => Permission::WorkflowsPublish,
        "jobs:read" => Permission::JobsReadOrganization,
        "jobs:write" => Permission::JobsWrite,
        "jobs:cancel" => Permission::JobsCancelOwn,
        "artifacts:read" => Permission::ArtifactsRead,
        "artifacts:write" => Permission::ArtifactsWrite,
        "workers:manage" => Permission::WorkersManage,
        "members:manage" => Permission::MembersManage,
        "api_keys:manage" => Permission::ApiKeysManageOwn,
        "quota:read" => Permission::QuotaRead,
        "quota:manage" => Permission::QuotaManage,
        "audit:read" => Permission::AuditRead,
        "devices:read" => Permission::DevicesRead,
        "devices:use" => Permission::DevicesUse,
        "devices:register" => Permission::DevicesRegisterOwn,
        "devices:share" => Permission::DevicesShareOwn,
        _ => return None,
    })
}
// The explicit fields keep every security-sensitive audit call site readable;
// collapsing them into an untyped map would make omissions easier.
#[allow(clippy::too_many_arguments)]
pub(super) async fn audit(
    state: &AppState,
    organization_id: Option<&str>,
    actor_id: Option<&str>,
    actor_kind: &str,
    request_id: &str,
    action: &str,
    resource_type: &str,
    resource_id: Option<&str>,
    outcome: &str,
    metadata: JsonValue,
) {
    let Some(store) = state.store.as_ref() else {
        return;
    };
    let metadata = serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".into());
    if let Err(error) = store
        .audit(AuditInsert {
            organization_id,
            actor_id,
            actor_kind: Some(actor_kind),
            request_id: Some(request_id),
            action,
            resource_type,
            resource_id,
            outcome,
            metadata_json: &metadata,
            created_at: now_unix_ms(),
        })
        .await
    {
        warn!(?error,%action,"failed to persist audit log")
    }
}
