use super::{
    authentication::{authenticate, authorize, require_browser},
    shared::*,
    *,
};

pub(super) async fn list_organizations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let store = store(&state).expect("authenticated browser requires store");
    match store
        .memberships_for_user(auth.principal.user_id.as_deref().unwrap_or_default())
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => product_error(map_store(error), &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateOrganizationRequest {
    name: String,
}
pub(super) async fn create_organization(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateOrganizationRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return product_error(
            HubError::InvalidRequest("organization name must contain 1-120 characters".into()),
            &request_id,
        );
    }
    let user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    let store = store(&state).expect("authenticated browser requires store");
    match store.create_organization_for_user(user_id, name).await {
        Ok(membership) => {
            audit(
                &state,
                Some(&membership.organization_id),
                Some(user_id),
                "browser_session",
                &request_id,
                "organization.create",
                "organization",
                Some(&membership.organization_id),
                "success",
                json!({"name": name}),
            )
            .await;
            (StatusCode::CREATED, Json(membership)).into_response()
        }
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

pub(super) async fn export_organization(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    if let Err(error) = authorize(&state, &headers, &org_id, Permission::AuditRead).await {
        return product_error(error, &request_id);
    }
    let store = match store(&state) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let organization = match store.organization(&org_id).await {
        Ok(Some(value)) => value,
        Ok(None) => return product_error(HubError::NotFound("organization".into()), &request_id),
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let data = match tokio::try_join!(
        store.members_for_org(&org_id),
        store.organization_invites(&org_id),
        store.api_keys_for_org(&org_id),
        store.worker_credentials_for_org(&org_id),
        store.devices_for_org(&org_id),
        store.quota(&org_id),
        store.workflows_for_org(&org_id),
        store.artifacts(&org_id),
        store.jobs_for_org(&org_id),
        store.events_for_org(&org_id),
        store.audit_logs_all(&org_id),
    ) {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let (
        members,
        organization_invites,
        api_keys,
        worker_credentials,
        devices,
        quota,
        workflows,
        artifacts,
        jobs,
        events,
        audit_logs,
    ) = data;
    Json(json!({
        "schema_version": 1,
        "exported_at": now_unix_ms(),
        "organization": organization,
        "members": members,
        "organization_invites": organization_invites,
        "api_keys": api_keys,
        "worker_credentials": worker_credentials,
        "devices": devices,
        "quota": quota,
        "workflows": workflows,
        "artifacts": artifacts,
        "jobs": jobs,
        "events": events,
        "audit_logs": audit_logs,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct DeleteOrganizationRequest {
    confirm: String,
}

pub(super) async fn delete_organization(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(request): Json<DeleteOrganizationRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&org_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if auth.principal.role != Role::Owner {
        return product_error(
            HubError::Forbidden("only an owner can delete an organization".into()),
            &request_id,
        );
    }
    if request.confirm.trim() != org_id {
        return product_error(
            HubError::InvalidRequest("confirm must exactly match the organization id".into()),
            &request_id,
        );
    }
    let store = match store(&state) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let object_keys = match store.organization_object_keys(&org_id).await {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    for object_key in object_keys {
        if let Err(error) = state.objects.delete(&object_key).await {
            return product_error(HubError::ObjectStore(error.to_string()), &request_id);
        }
    }
    state.sessions.disconnect_organization(&org_id).await;
    match store.delete_organization(&org_id).await {
        Ok(true) => {
            state.data.write().await.remove_organization(&org_id);
            state
                .invalidate_cached_device_access_for_organization(&org_id)
                .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => product_error(HubError::NotFound("organization".into()), &request_id),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

pub(super) async fn list_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    if let Err(error) = authorize(&state, &headers, &org_id, Permission::MembersManage).await {
        return product_error(error, &request_id);
    }
    match store(&state).unwrap().members_for_org(&org_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ChangeRoleRequest {
    role: Role,
}
pub(super) async fn change_member_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, user_id)): Path<(String, String)>,
    Json(request): Json<ChangeRoleRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize(&state, &headers, &org_id, Permission::MembersManage).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if request.role == Role::Owner && auth.principal.role != Role::Owner {
        return product_error(
            HubError::Forbidden("only an owner can grant owner role".into()),
            &request_id,
        );
    }
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    let target = match store.membership(&org_id, &user_id).await {
        Ok(Some(value)) => value,
        Ok(None) => return product_error(HubError::NotFound("member".into()), &request_id),
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    if target.role == Role::Owner && auth.principal.role != Role::Owner {
        return product_error(
            HubError::Forbidden("only an owner can change another owner's role".into()),
            &request_id,
        );
    }
    match store.set_member_role(&org_id, &user_id, request.role).await {
        Ok(true) => {
            audit(
                &state,
                Some(&org_id),
                auth.principal.user_id.as_deref(),
                auth_kind(auth.principal.kind),
                &request_id,
                "member.role.update",
                "user",
                Some(&user_id),
                "success",
                json!({"role": request.role}),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => product_error(HubError::NotFound("member".into()), &request_id),
        Err(error) => product_error(map_store(error), &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateOrganizationInviteRequest {
    #[serde(default = "default_organization_invite_role")]
    role:               Role,
    #[serde(default)]
    expires_in_seconds: Option<i64>,
}

const fn default_organization_invite_role() -> Role {
    Role::Member
}

#[derive(Debug, Serialize)]
pub(super) struct CreatedOrganizationInvite {
    invite:    nagisalake_hub_store::OrganizationInvite,
    plaintext: String,
}

pub(super) async fn list_organization_invites(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    if let Err(error) = authorize(&state, &headers, &org_id, Permission::MembersManage).await {
        return product_error(error, &request_id);
    }
    match store(&state).unwrap().organization_invites(&org_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

pub(super) async fn create_organization_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(request): Json<CreateOrganizationInviteRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize(&state, &headers, &org_id, Permission::MembersManage).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if request.role == Role::Owner {
        return product_error(
            HubError::InvalidRequest("organization invites cannot grant owner role".into()),
            &request_id,
        );
    }
    let inviter = auth.principal.user_id.as_deref().unwrap_or_default();
    let secret = generate_secret("noi");
    let id = Uuid::new_v4().to_string();
    let now = now_unix_ms();
    let expires_at = now.saturating_add(
        request
            .expires_in_seconds
            .unwrap_or(7 * 24 * 60 * 60)
            .clamp(300, 2_592_000)
            .saturating_mul(1_000),
    );
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    if let Err(error) = store
        .create_organization_invite(NewOrganizationInvite {
            id: &id,
            organization_id: &org_id,
            inviter_user_id: inviter,
            code_prefix: &secret.display_prefix,
            code_hash: &secret.hash,
            role: request.role,
            created_at: now,
            expires_at,
        })
        .await
    {
        return product_error(map_store(error), &request_id);
    }
    let invite = match store
        .organization_invites(&org_id)
        .await
        .map(|invites| invites.into_iter().find(|invite| invite.id == id))
    {
        Ok(Some(value)) => value,
        Ok(None) => {
            return product_error(HubError::NotFound("created invitation".into()), &request_id);
        }
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    audit(
        &state,
        Some(&org_id),
        Some(inviter),
        auth_kind(auth.principal.kind),
        &request_id,
        "organization_invite.create",
        "organization_invite",
        Some(&id),
        "success",
        json!({"role":request.role,"expires_at":expires_at}),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(CreatedOrganizationInvite {
            invite,
            plaintext: secret.plaintext,
        }),
    )
        .into_response()
}

pub(super) async fn revoke_organization_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, invite_id)): Path<(String, String)>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize(&state, &headers, &org_id, Permission::MembersManage).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    match store(&state)
        .unwrap()
        .revoke_organization_invite(&org_id, &invite_id)
        .await
    {
        Ok(true) => {
            audit(
                &state,
                Some(&org_id),
                auth.principal.user_id.as_deref(),
                auth_kind(auth.principal.kind),
                &request_id,
                "organization_invite.revoke",
                "organization_invite",
                Some(&invite_id),
                "success",
                json!({}),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => product_error(
            HubError::NotFound("organization invitation".into()),
            &request_id,
        ),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct AcceptOrganizationInviteRequest {
    code: String,
}

pub(super) async fn accept_organization_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AcceptOrganizationInviteRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let code = request.code.trim();
    if !code.starts_with("noi_") || code.len() > 256 {
        return product_error(
            HubError::InvalidRequest("organization invitation code is invalid".into()),
            &request_id,
        );
    }
    let user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    match store(&state)
        .unwrap()
        .accept_organization_invite(&hash_secret(code), user_id)
        .await
    {
        Ok(membership) => {
            audit(
                &state,
                Some(&membership.organization_id),
                Some(user_id),
                auth_kind(auth.principal.kind),
                &request_id,
                "organization_invite.accept",
                "organization_invite",
                None,
                "success",
                json!({"organization_id":membership.organization_id}),
            )
            .await;
            Json(membership).into_response()
        }
        Err(error) => product_error(map_store(error), &request_id),
    }
}

pub(super) async fn remove_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, user_id)): Path<(String, String)>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize(&state, &headers, &org_id, Permission::MembersManage).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if auth.principal.user_id.as_deref() == Some(user_id.as_str()) {
        return product_error(
            HubError::Conflict("use owner transfer before removing your own membership".into()),
            &request_id,
        );
    }
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    let target = match store.membership(&org_id, &user_id).await {
        Ok(Some(value)) => value,
        Ok(None) => return product_error(HubError::NotFound("member".into()), &request_id),
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    if target.role == Role::Owner && auth.principal.role != Role::Owner {
        return product_error(
            HubError::Forbidden("only an owner can remove another owner".into()),
            &request_id,
        );
    }
    match store.remove_member(&org_id, &user_id).await {
        Ok(Some(credential_ids)) => {
            for credential_id in credential_ids {
                state.sessions.disconnect_credential(&credential_id).await;
            }
            audit(
                &state,
                Some(&org_id),
                auth.principal.user_id.as_deref(),
                auth_kind(auth.principal.kind),
                &request_id,
                "member.remove",
                "user",
                Some(&user_id),
                "success",
                json!({"role":target.role}),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(None) => product_error(HubError::NotFound("member".into()), &request_id),
        Err(error) => product_error(map_store(error), &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct TransferOwnerRequest {
    user_id: String,
}

pub(super) async fn transfer_organization_owner(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(request): Json<TransferOwnerRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&org_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if auth.principal.role != Role::Owner {
        return product_error(
            HubError::Forbidden("only an owner can transfer ownership".into()),
            &request_id,
        );
    }
    let from_user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    match store(&state)
        .unwrap()
        .transfer_owner(&org_id, from_user_id, request.user_id.trim())
        .await
    {
        Ok(()) => {
            audit(
                &state,
                Some(&org_id),
                Some(from_user_id),
                auth_kind(auth.principal.kind),
                &request_id,
                "organization.owner.transfer",
                "user",
                Some(request.user_id.trim()),
                "success",
                json!({}),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => product_error(map_store(error), &request_id),
    }
}

pub(super) async fn list_api_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Query(query): Query<CursorQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authenticate(&state, &headers, Some(&org_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if auth.principal.kind != PrincipalKind::BrowserSession
        || !auth.principal.role.allows(Permission::ApiKeysManageOwn)
    {
        return product_error(
            HubError::Forbidden("browser member access is required".into()),
            &request_id,
        );
    }
    let limit = page_limit(query.limit);
    let after = match query.cursor.as_deref() {
        Some(cursor) => match decode_created_id_cursor(cursor) {
            Ok(value) => Some(value),
            Err(error) => return product_error(error, &request_id),
        },
        None => None,
    };
    let owner_user_id = (!auth.principal.role.allows(Permission::ApiKeysManage))
        .then_some(auth.principal.user_id.as_deref())
        .flatten();
    let mut keys = match store(&state)
        .unwrap()
        .api_keys_page(
            &org_id,
            owner_user_id,
            limit + 1,
            after
                .as_ref()
                .map(|(created_at, id)| (*created_at, id.as_str())),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let has_more = keys.len() > limit as usize;
    if has_more {
        keys.pop();
    }
    let next_cursor = has_more
        .then(|| keys.last())
        .flatten()
        .map(|key| encode_created_id_cursor(key.created_at, &key.id));
    Json(ListPage {
        items: keys,
        next_cursor,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateApiKeyRequest {
    name:               String,
    scopes:             Vec<String>,
    #[serde(default)]
    expires_in_seconds: Option<i64>,
}
#[derive(Debug, Serialize)]
pub(super) struct CreatedApiKey {
    key:       ApiKey,
    plaintext: String,
}
pub(super) async fn create_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(request): Json<CreateApiKeyRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&org_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.role.allows(Permission::ApiKeysManageOwn) {
        return product_error(
            HubError::Forbidden("role cannot create API keys".into()),
            &request_id,
        );
    }
    let scopes = match validate_scopes(&auth.principal, &request.scopes) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return product_error(
            HubError::InvalidRequest("API key name must contain 1-120 characters".into()),
            &request_id,
        );
    }
    let secret = generate_secret("nsk");
    let id = Uuid::new_v4().to_string();
    let now = now_unix_ms();
    let expires_at = request
        .expires_in_seconds
        .map(|seconds| now + seconds.clamp(60, 31_536_000) * 1_000);
    let scopes_json = serde_json::to_string(&scopes).unwrap_or_else(|_| "[]".into());
    let creator = auth.principal.user_id.as_deref().unwrap_or_default();
    let store = store(&state).unwrap();
    if let Err(error) = store
        .create_api_key(NewApiKey {
            id: &id,
            organization_id: &org_id,
            creator_user_id: creator,
            name,
            prefix: &secret.display_prefix,
            key_hash: &secret.hash,
            scopes: &scopes_json,
            created_at: now,
            expires_at,
        })
        .await
    {
        return product_error(HubError::Store(error), &request_id);
    }
    let key = match store.api_key_by_hash(&secret.hash).await {
        Ok(Some(value)) => value,
        Ok(None) => {
            return product_error(
                HubError::Store(StoreError::NotFound("created api key".into())),
                &request_id,
            );
        }
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    audit(
        &state,
        Some(&org_id),
        Some(creator),
        "browser_session",
        &request_id,
        "api_key.create",
        "api_key",
        Some(&id),
        "success",
        json!({"name": name, "scopes": scopes}),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(CreatedApiKey {
            key,
            plaintext: secret.plaintext,
        }),
    )
        .into_response()
}

pub(super) async fn revoke_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, key_id)): Path<(String, String)>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&org_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.role.allows(Permission::ApiKeysManageOwn) {
        return product_error(
            HubError::Forbidden("role cannot revoke API keys".into()),
            &request_id,
        );
    }
    let store = store(&state).unwrap();
    let key = match store.api_key_for_org(&org_id, &key_id).await {
        Ok(Some(value)) => value,
        Ok(None) => return product_error(HubError::NotFound("api key".into()), &request_id),
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    if !auth.principal.role.allows(Permission::ApiKeysManage)
        && Some(key.creator_user_id.as_str()) != auth.principal.user_id.as_deref()
    {
        return product_error(
            HubError::Forbidden("role cannot revoke another user's API key".into()),
            &request_id,
        );
    }
    match store.revoke_api_key(&org_id, &key_id).await {
        Ok(true) => {
            audit(
                &state,
                Some(&org_id),
                auth.principal.user_id.as_deref(),
                "browser_session",
                &request_id,
                "api_key.revoke",
                "api_key",
                Some(&key_id),
                "success",
                json!({}),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => product_error(HubError::NotFound("api key".into()), &request_id),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

pub(super) async fn get_quota(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    if let Err(error) = authorize(&state, &headers, &org_id, Permission::QuotaRead).await {
        return product_error(error, &request_id);
    }
    match store(&state).unwrap().quota(&org_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateQuotaRequest {
    max_concurrent_jobs: i64,
    max_storage_bytes:   i64,
    max_jobs_per_period: i64,
    period_seconds:      i64,
}

pub(super) async fn update_quota(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(request): Json<UpdateQuotaRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize(&state, &headers, &org_id, Permission::QuotaManage).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !(1..=10_000).contains(&request.max_concurrent_jobs)
        || !(1..=5_i64 * 1024 * 1024 * 1024 * 1024).contains(&request.max_storage_bytes)
        || !(1..=1_000_000_000).contains(&request.max_jobs_per_period)
        || !(60..=31_536_000).contains(&request.period_seconds)
    {
        return product_error(
            HubError::InvalidRequest(
                "quota values are outside the allowed production bounds".into(),
            ),
            &request_id,
        );
    }
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    match store
        .update_quota_policy(QuotaPolicyUpdate {
            organization_id:     &org_id,
            max_concurrent_jobs: request.max_concurrent_jobs,
            max_storage_bytes:   request.max_storage_bytes,
            max_jobs_per_period: request.max_jobs_per_period,
            period_seconds:      request.period_seconds,
        })
        .await
    {
        Ok(value) => {
            audit(
                &state,
                Some(&org_id),
                auth.principal.user_id.as_deref(),
                auth_kind(auth.principal.kind),
                &request_id,
                "quota_policy.update",
                "quota_policy",
                Some(&org_id),
                "success",
                json!({
                    "max_concurrent_jobs": request.max_concurrent_jobs,
                    "max_storage_bytes": request.max_storage_bytes,
                    "max_jobs_per_period": request.max_jobs_per_period,
                    "period_seconds": request.period_seconds,
                }),
            )
            .await;
            Json(value).into_response()
        }
        Err(error) => product_error(map_store(error), &request_id),
    }
}

pub(super) async fn list_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Query(query): Query<CursorQuery>,
) -> Response {
    let request_id = request_id(&headers);
    if let Err(error) = authorize(&state, &headers, &org_id, Permission::AuditRead).await {
        return product_error(error, &request_id);
    }
    let limit = page_limit(query.limit);
    let after = match query.cursor.as_deref() {
        Some(cursor) => match decode_created_id_cursor(cursor) {
            Ok(value) => Some(value),
            Err(error) => return product_error(error, &request_id),
        },
        None => None,
    };
    let mut logs = match store(&state)
        .unwrap()
        .audit_logs_page(
            &org_id,
            limit + 1,
            after
                .as_ref()
                .map(|(created_at, id)| (*created_at, id.as_str())),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let has_more = logs.len() > limit as usize;
    if has_more {
        logs.pop();
    }
    let next_cursor = has_more
        .then(|| logs.last())
        .flatten()
        .map(|log| encode_created_id_cursor(log.created_at, &log.id));
    Json(ListPage {
        items: logs,
        next_cursor,
    })
    .into_response()
}

pub(super) async fn list_worker_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Query(query): Query<CursorQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&org_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.role.allows(Permission::DevicesRegisterOwn) {
        return product_error(
            HubError::Forbidden("role cannot register devices".into()),
            &request_id,
        );
    }
    let limit = page_limit(query.limit);
    let after = match query.cursor.as_deref() {
        Some(cursor) => match decode_id_cursor(cursor) {
            Ok(value) => Some(value),
            Err(error) => return product_error(error, &request_id),
        },
        None => None,
    };
    let owner_user_id = (!auth.principal.role.allows(Permission::WorkersManage))
        .then_some(auth.principal.user_id.as_deref())
        .flatten();
    let mut values = match store(&state)
        .unwrap()
        .worker_credentials_page(&org_id, owner_user_id, limit + 1, after.as_deref())
        .await
    {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let has_more = values.len() > limit as usize;
    if has_more {
        values.pop();
    }
    let next_cursor = has_more
        .then(|| values.last())
        .flatten()
        .map(|credential| encode_id_cursor(&credential.id));
    Json(ListPage {
        items: values,
        next_cursor,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateWorkerCredentialRequest {
    name:               String,
    #[serde(default)]
    allowed_namespace:  Option<String>,
    #[serde(default)]
    expires_in_seconds: Option<i64>,
}
#[derive(Debug, Serialize)]
pub(super) struct CreatedWorkerCredential {
    credential: WorkerCredential,
    plaintext:  String,
}
pub(super) async fn create_worker_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(request): Json<CreateWorkerCredentialRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&org_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.role.allows(Permission::DevicesRegisterOwn) {
        return product_error(
            HubError::Forbidden("role cannot register devices".into()),
            &request_id,
        );
    }
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return product_error(
            HubError::InvalidRequest("credential name must contain 1-120 characters".into()),
            &request_id,
        );
    }
    let secret = generate_secret("nwk");
    let id = Uuid::new_v4().to_string();
    let now = now_unix_ms();
    let expires_at = request
        .expires_in_seconds
        .map(|seconds| now + seconds.clamp(300, 31_536_000) * 1_000);
    let owner = auth.principal.user_id.as_deref().unwrap_or_default();
    let store = store(&state).unwrap();
    if let Err(error) = store
        .create_worker_credential(NewWorkerCredential {
            id: &id,
            organization_id: &org_id,
            owner_user_id: Some(owner),
            name,
            token_prefix: &secret.display_prefix,
            token_hash: &secret.hash,
            allowed_namespace: request.allowed_namespace.as_deref(),
            created_at: now,
            expires_at,
        })
        .await
    {
        return product_error(HubError::Store(error), &request_id);
    }
    let credential = match store.worker_credential_by_hash(&secret.hash).await {
        Ok(Some(value)) => value,
        Ok(None) => {
            return product_error(HubError::NotFound("created credential".into()), &request_id);
        }
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    audit(
        &state,
        Some(&org_id),
        Some(owner),
        "browser_session",
        &request_id,
        "worker_credential.create",
        "worker_credential",
        Some(&id),
        "success",
        json!({"allowed_namespace":request.allowed_namespace}),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(CreatedWorkerCredential {
            credential,
            plaintext: secret.plaintext,
        }),
    )
        .into_response()
}

pub(super) async fn revoke_worker_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, credential_id)): Path<(String, String)>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, Some(&org_id)).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.role.allows(Permission::DevicesRegisterOwn) {
        return product_error(
            HubError::Forbidden("role cannot revoke device credentials".into()),
            &request_id,
        );
    }
    let store = store(&state).unwrap();
    let credential = match store
        .worker_credential_for_org(&org_id, &credential_id)
        .await
    {
        Ok(Some(value)) => value,
        Ok(None) => {
            return product_error(HubError::NotFound("worker credential".into()), &request_id);
        }
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    if !auth.principal.role.allows(Permission::WorkersManage)
        && credential.owner_user_id.as_deref() != auth.principal.user_id.as_deref()
    {
        return product_error(
            HubError::Forbidden("role cannot revoke another user's device credential".into()),
            &request_id,
        );
    }
    match store
        .revoke_worker_credential(&org_id, &credential_id)
        .await
    {
        Ok(true) => {
            state.sessions.disconnect_credential(&credential_id).await;
            audit(
                &state,
                Some(&org_id),
                auth.principal.user_id.as_deref(),
                "browser_session",
                &request_id,
                "worker_credential.revoke",
                "worker_credential",
                Some(&credential_id),
                "success",
                json!({}),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => product_error(HubError::NotFound("worker credential".into()), &request_id),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}
