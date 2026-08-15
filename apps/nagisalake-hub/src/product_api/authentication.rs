use super::{shared::*, *};

pub(super) const REFRESH_COOKIE: &str = "nagisalake_refresh";
pub(super) const CSRF_COOKIE: &str = "nagisalake_csrf";

#[derive(Debug, Deserialize)]
pub(super) struct RegisterRequest {
    email:             String,
    password:          String,
    #[serde(default)]
    organization_name: Option<String>,
}

pub(super) async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(request): Json<RegisterRequest>,
) -> Response {
    let request_id = request_id(&headers);
    if !state.config.browser.password_auth_enabled {
        return product_error(
            HubError::Forbidden("password registration is disabled; use OAuth".into()),
            &request_id,
        );
    }
    if !state.config.browser.registration_enabled {
        return product_error(
            HubError::Forbidden("registration is disabled".into()),
            &request_id,
        );
    }
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    if let Err(error) = state
        .rate_limit_ip(
            &headers,
            peer.map(|value| value.0.0.ip()),
            "auth.register",
            state.rate_limiter.limits().register_per_ip,
        )
        .await
    {
        return product_error(error, &request_id);
    }
    if !valid_email(&request.email) {
        return product_error(
            HubError::InvalidRequest("a valid email address is required".into()),
            &request_id,
        );
    }
    let password_hash = match hash_password_async(request.password.clone()).await {
        Ok(hash) => hash,
        Err(error) => {
            return product_error(HubError::InvalidRequest(error.to_string()), &request_id);
        }
    };
    let organization_name = request
        .organization_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{} workspace",
                request.email.split('@').next().unwrap_or("My")
            )
        });
    if organization_name.chars().count() > 120 {
        return product_error(
            HubError::InvalidRequest("organization name must contain 1-120 characters".into()),
            &request_id,
        );
    }
    let account = match store
        .register_user(&request.email, &password_hash, &organization_name)
        .await
    {
        Ok(account) => account,
        Err(StoreError::Conflict(_)) => {
            return product_error(
                HubError::Conflict("email is already registered".into()),
                &request_id,
            );
        }
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let tokens = match issue_session(
        &state,
        store,
        &headers,
        &account.user.id,
        &account.organization_id,
    )
    .await
    {
        Ok(tokens) => tokens,
        Err(error) => return product_error(error, &request_id),
    };
    audit(
        &state,
        Some(&account.organization_id),
        Some(&account.user.id),
        "browser_session",
        &request_id,
        "auth.register",
        "user",
        Some(&account.user.id),
        "success",
        json!({"email": account.user.email}),
    )
    .await;
    auth_response(
        &state,
        StatusCode::CREATED,
        account.user,
        account.organization_id,
        tokens,
    )
}

#[derive(Debug, Deserialize)]
pub(super) struct LoginRequest {
    email:           String,
    password:        String,
    #[serde(default)]
    organization_id: Option<String>,
}

pub(super) async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(request): Json<LoginRequest>,
) -> Response {
    let request_id = request_id(&headers);
    if !state.config.browser.password_auth_enabled {
        return product_error(
            HubError::Forbidden("password login is disabled; use OAuth".into()),
            &request_id,
        );
    }
    let peer_ip = peer.map(|value| value.0.0.ip());
    if let Err(error) = state
        .rate_limit_ip(
            &headers,
            peer_ip,
            "auth.login.ip",
            state.rate_limiter.limits().login_per_ip,
        )
        .await
    {
        return product_error(error, &request_id);
    }
    let account_key = hash_secret(&request.email.trim().to_ascii_lowercase());
    if let Err(error) = state
        .rate_limit_key(
            "auth.login.account",
            &account_key,
            state.rate_limiter.limits().login_per_account,
        )
        .await
    {
        return product_error(error, &request_id);
    }
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    let user = match store.user_by_email(&request.email).await {
        Ok(user) => user,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    // Always spend one Argon2 verification, even when the address is unknown.
    // Returning early there would make the response time reveal whether an
    // account exists, which is enough to enumerate registered addresses.
    // `and_then` flattens the federated case: an account with no password hash
    // is treated exactly like a missing account — the dummy hash is verified and
    // the answer is false. That refuses password sign-in for a federated-only
    // account without revealing that it exists.
    let password_matches = verify_password_async(
        request.password.clone(),
        user.as_ref().and_then(|user| user.password_hash.clone()),
    )
    .await;
    let Some(user) = user else {
        return product_error(
            HubError::Unauthorized("invalid email or password".into()),
            &request_id,
        );
    };
    let now = now_unix_ms();
    let locked_until = match store.login_lock_until(&user.id, now).await {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    if let Some(until) = locked_until {
        return product_error(
            HubError::RateLimited {
                retry_after_seconds: until.saturating_sub(now).div_euclid(1_000).max(1) as u64,
            },
            &request_id,
        );
    }
    if user.status != "active" || !password_matches {
        let locked_until = match store.record_failed_login(&user.id, now, 10, 15 * 60).await {
            Ok(value) => value,
            Err(error) => return product_error(HubError::Store(error), &request_id),
        };
        audit(
            &state,
            None,
            Some(&user.id),
            "browser_session",
            &request_id,
            "auth.login",
            "user",
            Some(&user.id),
            "denied",
            json!({}),
        )
        .await;
        if let Some(until) = locked_until {
            return product_error(
                HubError::RateLimited {
                    retry_after_seconds: until
                        .saturating_sub(now_unix_ms())
                        .div_euclid(1_000)
                        .max(1) as u64,
                },
                &request_id,
            );
        }
        return product_error(
            HubError::Unauthorized("invalid email or password".into()),
            &request_id,
        );
    }
    let memberships = match store.memberships_for_user(&user.id).await {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let organization_id = request
        .organization_id
        .as_deref()
        .and_then(|wanted| {
            memberships
                .iter()
                .find(|membership| membership.organization_id == wanted)
        })
        .or_else(|| memberships.first())
        .map(|membership| membership.organization_id.clone());
    let Some(organization_id) = organization_id else {
        return product_error(
            HubError::Forbidden("user has no active organization".into()),
            &request_id,
        );
    };
    if let Err(error) = store.clear_failed_logins(&user.id).await {
        return product_error(HubError::Store(error), &request_id);
    }
    state
        .rate_limiter
        .reset("auth.login.account", &account_key)
        .await;
    state
        .rate_limiter
        .reset(
            "auth.login.ip",
            &crate::ratelimit::client_address(
                &headers,
                peer_ip,
                state.config.rate_limit.trust_forwarded_for,
            ),
        )
        .await;
    let tokens = match issue_session(&state, store, &headers, &user.id, &organization_id).await {
        Ok(tokens) => tokens,
        Err(error) => return product_error(error, &request_id),
    };
    audit(
        &state,
        Some(&organization_id),
        Some(&user.id),
        "browser_session",
        &request_id,
        "auth.login",
        "session",
        Some(&tokens.session_id),
        "success",
        json!({}),
    )
    .await;
    auth_response(&state, StatusCode::OK, user, organization_id, tokens)
}

pub(super) async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Response {
    let request_id = request_id(&headers);
    if let Err(error) = state
        .rate_limit_ip(
            &headers,
            peer.map(|value| value.0.0.ip()),
            "auth.refresh",
            state.rate_limiter.limits().refresh_per_ip,
        )
        .await
    {
        return product_error(error, &request_id);
    }
    if let Err(error) = verify_origin(&state, &headers) {
        return product_error(error, &request_id);
    }
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    let cookies = cookies(&headers);
    let Some(refresh) = cookies.get(REFRESH_COOKIE) else {
        return product_error(
            HubError::Unauthorized("refresh cookie is required".into()),
            &request_id,
        );
    };
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok());
    let csrf_cookie = cookies.get(CSRF_COOKIE).copied();
    if csrf.is_none() || csrf != csrf_cookie {
        return product_error(
            HubError::Forbidden("CSRF token is missing or invalid".into()),
            &request_id,
        );
    }
    let Some(session) = (match store.session_by_refresh_hash(&hash_secret(refresh)).await {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    }) else {
        return product_error(
            HubError::Unauthorized("refresh session is invalid".into()),
            &request_id,
        );
    };
    let now = now_unix_ms();
    if session.revoked_at.is_some()
        || session.refresh_expires_at <= now
        || !verify_secret(csrf.unwrap_or_default(), &session.csrf_token_hash)
    {
        return product_error(
            HubError::Unauthorized("refresh session is expired or revoked".into()),
            &request_id,
        );
    }
    let access = generate_secret("nss");
    let next_refresh = generate_secret("nsr");
    let access_expires_at = now + state.config.browser.access_ttl_seconds * 1_000;
    let refresh_expires_at = now + state.config.browser.refresh_ttl_seconds * 1_000;
    let rotated = match store
        .rotate_session(RotateSession {
            session_id: &session.id,
            expected_refresh_token_hash: &hash_secret(refresh),
            access_token_hash: &access.hash,
            refresh_token_hash: &next_refresh.hash,
            now,
            access_expires_at,
            refresh_expires_at,
        })
        .await
    {
        Ok(rotated) => rotated,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    if !rotated {
        return product_error(
            HubError::Unauthorized("refresh token was already rotated".into()),
            &request_id,
        );
    }
    let Some(user) = (match store.user_by_id(&session.user_id).await {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    }) else {
        return product_error(
            HubError::Unauthorized("session user no longer exists".into()),
            &request_id,
        );
    };
    auth_response(
        &state,
        StatusCode::OK,
        user,
        session.organization_id,
        SessionTokens {
            session_id: session.id,
            access,
            refresh: next_refresh,
            csrf: GeneratedSecret {
                plaintext:      csrf.unwrap_or_default().into(),
                display_prefix: String::new(),
                hash:           session.csrf_token_hash,
            },
            access_expires_at,
            refresh_expires_at,
        },
    )
}

pub(super) async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(auth) => auth,
        Err(error) => return product_error(error, &request_id),
    };
    let store = store(&state).expect("authenticated browser requires store");
    if let Err(error) = store
        .revoke_session(auth.session_id.as_deref().unwrap_or_default())
        .await
    {
        return product_error(HubError::Store(error), &request_id);
    }
    audit(
        &state,
        Some(&auth.principal.organization_id),
        auth.principal.user_id.as_deref(),
        "browser_session",
        &request_id,
        "auth.logout",
        "session",
        auth.session_id.as_deref(),
        "success",
        json!({}),
    )
    .await;
    clear_auth_cookies(&state, StatusCode::NO_CONTENT)
}

pub(super) async fn revoke_all_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(auth) => auth,
        Err(error) => return product_error(error, &request_id),
    };
    let user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    let store = store(&state).expect("authenticated browser requires store");
    if let Err(error) = store.revoke_user_sessions(user_id, None).await {
        return product_error(HubError::Store(error), &request_id);
    }
    audit(
        &state,
        Some(&auth.principal.organization_id),
        Some(user_id),
        "browser_session",
        &request_id,
        "auth.sessions.revoke_all",
        "user",
        Some(user_id),
        "success",
        json!({}),
    )
    .await;
    clear_auth_cookies(&state, StatusCode::NO_CONTENT)
}

pub(super) async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(auth) => auth,
        Err(error) => return product_error(error, &request_id),
    };
    let user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    let store = store(&state).expect("authentication requires store");
    let user = match store.user_by_id(user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return product_error(HubError::NotFound("user".into()), &request_id),
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let memberships = match store.memberships_for_user(user_id).await {
        Ok(value) => value,
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    Json(json!({"user": public_user(&user), "current_organization_id": auth.principal.organization_id, "memberships": memberships, "auth_kind": auth_kind(auth.principal.kind)})).into_response()
}

pub(super) async fn delete_account(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    let store = match store(&state) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let credential_ids = match store.delete_user(user_id).await {
        Ok(value) => value,
        Err(error) => return product_error(map_store(error), &request_id),
    };
    for credential_id in credential_ids {
        state.sessions.disconnect_credential(&credential_id).await;
    }
    clear_auth_cookies(&state, StatusCode::NO_CONTENT)
}

pub(super) async fn authorize_current(
    state: &AppState,
    headers: &HeaderMap,
    permission: Permission,
) -> Result<AuthContext, HubError> {
    let auth = authenticate(state, headers, None).await?;
    if !auth.principal.allows(permission) {
        return Err(HubError::Forbidden(format!(
            "missing permission {}",
            permission.scope()
        )));
    }
    Ok(auth)
}

#[derive(Debug, Clone)]
pub(super) struct AuthContext {
    pub principal:  Principal,
    pub session_id: Option<String>,
}

pub(super) async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    organization_id: Option<&str>,
) -> Result<AuthContext, HubError> {
    let token = bearer_token(headers)
        .ok_or_else(|| HubError::Unauthorized("bearer token is required".into()))?;
    let store = store(state)?;
    let now = now_unix_ms();
    let header_organization = headers
        .get("x-organization-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty());
    let requested_organization = organization_id.or(header_organization);
    if token.starts_with("nss_") {
        let session = store
            .session_by_access_hash(&hash_secret(&token))
            .await
            .map_err(HubError::Store)?
            .ok_or_else(|| HubError::Unauthorized("invalid browser session".into()))?;
        if session.revoked_at.is_some() || session.access_expires_at <= now {
            return Err(HubError::Unauthorized(
                "browser session is expired or revoked".into(),
            ));
        }
        let org = requested_organization.unwrap_or(&session.organization_id);
        let membership = store
            .membership(org, &session.user_id)
            .await
            .map_err(HubError::Store)?
            .ok_or_else(|| HubError::Forbidden("user is not an organization member".into()))?;
        return Ok(AuthContext {
            principal:  Principal {
                kind:            PrincipalKind::BrowserSession,
                actor_id:        session.id.clone(),
                user_id:         Some(session.user_id),
                organization_id: org.into(),
                role:            membership.role,
                scopes:          BTreeSet::new(),
            },
            session_id: Some(session.id),
        });
    }
    if token.starts_with("nsk_") {
        let key = store
            .api_key_by_hash(&hash_secret(&token))
            .await
            .map_err(HubError::Store)?
            .ok_or_else(|| HubError::Unauthorized("invalid API key".into()))?;
        if key.revoked_at.is_some() || key.expires_at.is_some_and(|expires| expires <= now) {
            return Err(HubError::Unauthorized(
                "API key is expired or revoked".into(),
            ));
        }
        store
            .touch_api_key(&key.id, now)
            .await
            .map_err(HubError::Store)?;
        if requested_organization.is_some_and(|org| org != key.organization_id) {
            return Err(HubError::NotFound("organization resource".into()));
        }
        let membership = store
            .membership(&key.organization_id, &key.creator_user_id)
            .await
            .map_err(HubError::Store)?
            .ok_or_else(|| {
                HubError::Unauthorized("API key creator is no longer a member".into())
            })?;
        return Ok(AuthContext {
            principal:  Principal {
                kind:            PrincipalKind::ApiKey,
                actor_id:        key.id,
                user_id:         Some(key.creator_user_id),
                organization_id: key.organization_id,
                role:            membership.role,
                scopes:          key.scopes.into_iter().collect(),
            },
            session_id: None,
        });
    }
    Err(HubError::Unauthorized(
        "unsupported bearer credential type".into(),
    ))
}

pub(super) async fn require_browser(
    state: &AppState,
    headers: &HeaderMap,
    org: Option<&str>,
) -> Result<AuthContext, HubError> {
    let auth = authenticate(state, headers, org).await?;
    if auth.principal.kind != PrincipalKind::BrowserSession {
        return Err(HubError::Forbidden("browser session is required".into()));
    }
    Ok(auth)
}
pub(super) async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    permission: Permission,
) -> Result<AuthContext, HubError> {
    let auth = authenticate(state, headers, Some(org)).await?;
    if !auth.principal.allows(permission) {
        return Err(HubError::Forbidden(format!(
            "missing permission {}",
            permission.scope()
        )));
    }
    Ok(auth)
}

#[derive(Debug)]
pub(super) struct SessionTokens {
    session_id:         String,
    access:             GeneratedSecret,
    refresh:            GeneratedSecret,
    csrf:               GeneratedSecret,
    access_expires_at:  i64,
    refresh_expires_at: i64,
}
/// Providers a browser may offer, without leaking any configuration detail.
#[derive(Debug, Serialize)]
pub(super) struct PublicProvider {
    pub(super) name: String,
    pub(super) kind: crate::oauth::ProviderKind,
}

pub(super) async fn list_oauth_providers(State(state): State<AppState>) -> Response {
    let providers = state
        .oauth_providers
        .keys()
        .map(|name| PublicProvider {
            name: name.clone(),
            kind: state.oauth_providers[name].kind,
        })
        .collect::<Vec<_>>();
    Json(json!({"providers": providers})).into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct OauthStartQuery {
    /// Where to land after signing in. Validated as a same-site path.
    #[serde(default)]
    redirect: Option<String>,
}

/// Begins a federated sign-in by redirecting to the provider.
///
/// The PKCE verifier stays server-side; only the derived challenge is sent. The
/// `state` value is stored so the callback can be matched to this request and
/// accepted exactly once.
pub(super) async fn start_oauth(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Path(provider_name): Path<String>,
    Query(query): Query<OauthStartQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let Some(provider) = state.oauth_providers.get(&provider_name) else {
        return product_error(
            HubError::NotFound(format!("OAuth provider {provider_name}")),
            &request_id,
        );
    };
    let Some(oauth) = state.config.oauth.as_ref() else {
        return product_error(
            HubError::Unavailable("OAuth is not configured".into()),
            &request_id,
        );
    };
    let store = match store(&state) {
        Ok(store) => store,
        Err(error) => return product_error(error, &request_id),
    };
    if let Err(error) = state
        .rate_limit_ip(
            &headers,
            peer.map(|value| value.0.0.ip()),
            "auth.oauth.start",
            state.rate_limiter.limits().oauth_per_ip,
        )
        .await
    {
        return product_error(error, &request_id);
    }

    let pkce = crate::oauth::generate_pkce();
    let csrf_state = crate::oauth::generate_state();
    let redirect_path = crate::oauth::sanitize_redirect_path(query.redirect.as_deref());
    if let Err(error) = store
        .create_oauth_authorization(
            &csrf_state,
            &provider_name,
            &pkce.verifier,
            &redirect_path,
            crate::oauth::AUTHORIZATION_TTL_SECONDS,
        )
        .await
    {
        return product_error(HubError::Store(error), &request_id);
    }
    let target = crate::oauth::authorize_url(
        provider,
        &oauth.redirect_uri(&provider_name),
        &csrf_state,
        &pkce.challenge,
    );
    match HeaderValue::from_str(&target) {
        Ok(location) => {
            let mut response = StatusCode::SEE_OTHER.into_response();
            response.headers_mut().insert(LOCATION, location);
            response
        }
        Err(_) => product_error(
            HubError::InvalidConfig("provider authorize URL is not a valid header".into()),
            &request_id,
        ),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct OauthCallbackQuery {
    #[serde(default)]
    code:              Option<String>,
    #[serde(default)]
    state:             Option<String>,
    /// Providers report user denial here rather than with an HTTP error.
    #[serde(default)]
    error:             Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Completes a federated sign-in and issues a browser session.
///
/// Redirects rather than returning JSON, because the browser arrives here by
/// navigation. Failures land on the sign-in page with a short reason; the access
/// token is delivered in the session cookie pair, never in the URL, so it cannot
/// end up in history or a referrer header.
pub(super) async fn oauth_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_name): Path<String>,
    Query(query): Query<OauthCallbackQuery>,
) -> Response {
    let request_id = request_id(&headers);
    if let Some(error) = query.error.as_deref() {
        let detail = query.error_description.as_deref().unwrap_or(error);
        warn!(provider = %provider_name, %error, "OAuth provider reported an error");
        return redirect_to_signin(&format!("provider_error:{detail}"));
    }
    let (Some(code), Some(csrf_state)) = (query.code.as_deref(), query.state.as_deref()) else {
        return redirect_to_signin("missing_code");
    };
    let Some(provider) = state.oauth_providers.get(&provider_name) else {
        return redirect_to_signin("unknown_provider");
    };
    let Some(oauth) = state.config.oauth.as_ref() else {
        return redirect_to_signin("oauth_disabled");
    };
    let store = match store(&state) {
        Ok(store) => store,
        Err(_) => return redirect_to_signin("control_plane_unavailable"),
    };

    // Single-use: a replayed callback finds the state already consumed.
    let authorization = match store.consume_oauth_authorization(csrf_state).await {
        Ok(Some(authorization)) => authorization,
        Ok(None) => return redirect_to_signin("expired_or_replayed"),
        Err(error) => {
            warn!(?error, "failed to consume the OAuth authorization");
            return redirect_to_signin("state_lookup_failed");
        }
    };
    // The state was minted for one provider; a mismatch means the callback was
    // crafted rather than issued by us.
    if authorization.provider != provider_name {
        return redirect_to_signin("provider_mismatch");
    }

    let identity = match crate::oauth::exchange_and_fetch_identity(
        &state.http_client,
        provider,
        &oauth.redirect_uri(&provider_name),
        code,
        &authorization.pkce_verifier,
    )
    .await
    {
        Ok(identity) => identity,
        Err(error) => {
            warn!(provider = %provider_name, ?error, "OAuth identity exchange failed");
            return redirect_to_signin("identity_exchange_failed");
        }
    };

    if !provider.allows_email(&identity.email) {
        return redirect_to_signin("email_domain_not_allowed");
    }
    if !identity.email_verified {
        return redirect_to_signin("email_not_verified_by_provider");
    }
    if !state.config.browser.registration_enabled {
        // Registration closed: only an already-linked account may sign in.
        match store
            .user_by_federated_identity(&provider_name, &identity.subject)
            .await
        {
            Ok(Some(user)) => {
                let login = nagisalake_hub_store::FederatedLogin {
                    user,
                    outcome: nagisalake_hub_store::FederatedOutcome::Existing,
                };
                return finish_oauth_session(
                    &state,
                    store,
                    &headers,
                    login,
                    &authorization.redirect_path,
                    &request_id,
                )
                .await;
            }
            Ok(None) => return redirect_to_signin("registration_closed"),
            Err(error) => {
                warn!(?error, "federated identity resolution failed");
                return redirect_to_signin("identity_rejected");
            }
        }
    }

    let organization_stem = identity
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| identity.email.split('@').next().unwrap_or("My"));
    let organization_name = format!("{} workspace", truncate(organization_stem, 100));
    let login = match store
        .resolve_federated_identity(
            &provider_name,
            &identity.subject,
            &identity.email,
            identity.email_verified,
            &organization_name,
        )
        .await
    {
        Ok(login) => login,
        Err(nagisalake_hub_store::StoreError::Conflict(message)) => {
            // The commonest case: the address exists locally but the provider
            // did not verify it, so linking would be an account takeover.
            warn!(provider = %provider_name, %message, "refused to link a federated identity");
            return redirect_to_signin("email_not_verified_by_provider");
        }
        Err(error) => {
            warn!(?error, "federated identity resolution failed");
            return redirect_to_signin("identity_rejected");
        }
    };
    finish_oauth_session(
        &state,
        store,
        &headers,
        login,
        &authorization.redirect_path,
        &request_id,
    )
    .await
}

/// Issues the session cookies and redirects into the console.
pub(super) async fn finish_oauth_session(
    state: &AppState,
    store: &PgStore,
    headers: &HeaderMap,
    login: nagisalake_hub_store::FederatedLogin,
    redirect_path: &str,
    request_id: &str,
) -> Response {
    if login.user.status != "active" {
        return redirect_to_signin("account_disabled");
    }
    let organization_id = match &login.outcome {
        nagisalake_hub_store::FederatedOutcome::Created { organization_id } => {
            organization_id.clone()
        }
        _ => match store.memberships_for_user(&login.user.id).await {
            Ok(memberships) => match memberships.first() {
                Some(membership) => membership.organization_id.clone(),
                None => return redirect_to_signin("no_organization"),
            },
            Err(error) => {
                warn!(?error, "failed to read memberships after federated sign-in");
                return redirect_to_signin("membership_lookup_failed");
            }
        },
    };
    let tokens = match issue_session(state, store, headers, &login.user.id, &organization_id).await
    {
        Ok(tokens) => tokens,
        Err(error) => {
            warn!(?error, "failed to issue a session after federated sign-in");
            return redirect_to_signin("session_issue_failed");
        }
    };
    audit(
        state,
        Some(&organization_id),
        Some(&login.user.id),
        "browser_session",
        request_id,
        "auth.oauth.login",
        "session",
        Some(&tokens.session_id),
        "success",
        json!({
            "outcome": match login.outcome {
                nagisalake_hub_store::FederatedOutcome::Existing => "existing",
                nagisalake_hub_store::FederatedOutcome::Linked => "linked",
                nagisalake_hub_store::FederatedOutcome::Created { .. } => "created",
            }
        }),
    )
    .await;

    // The console reads the session from the refresh cookie on load, so no token
    // needs to travel in the URL.
    let mut response = StatusCode::SEE_OTHER.into_response();
    if let Ok(location) = HeaderValue::from_str(redirect_path) {
        response.headers_mut().insert(LOCATION, location);
    }
    append_cookie(
        &mut response,
        refresh_cookie(
            state,
            &tokens.refresh.plaintext,
            state.config.browser.refresh_ttl_seconds,
            true,
        ),
    );
    append_cookie(
        &mut response,
        csrf_cookie(
            state,
            &tokens.csrf.plaintext,
            state.config.browser.refresh_ttl_seconds,
        ),
    );
    response
}

/// Sends the browser back to the sign-in page with a machine-readable reason.
fn redirect_to_signin(reason: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    let target = format!("/?oauth_error={}", urlencode_component(reason));
    if let Ok(location) = HeaderValue::from_str(&target) {
        response.headers_mut().insert(LOCATION, location);
    }
    response
}

fn urlencode_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Providers linked to the signed-in account.
pub(super) async fn list_linked_identities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(auth) => auth,
        Err(error) => return product_error(error, &request_id),
    };
    let store = store(&state).expect("authentication requires store");
    match store
        .identities_for_user(auth.principal.user_id.as_deref().unwrap_or_default())
        .await
    {
        Ok(identities) => Json(identities).into_response(),
        Err(error) => product_error(HubError::Store(error), &request_id),
    }
}

pub(super) async fn issue_session(
    state: &AppState,
    store: &PgStore,
    headers: &HeaderMap,
    user_id: &str,
    organization_id: &str,
) -> Result<SessionTokens, HubError> {
    let now = now_unix_ms();
    let access = generate_secret("nss");
    let refresh = generate_secret("nsr");
    let csrf = generate_secret("nsc");
    let session_id = Uuid::new_v4().to_string();
    let family_id = Uuid::new_v4().to_string();
    let access_expires_at = now + state.config.browser.access_ttl_seconds * 1_000;
    let refresh_expires_at = now + state.config.browser.refresh_ttl_seconds * 1_000;
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(hash_secret);
    store
        .create_session(NewSession {
            id: &session_id,
            user_id,
            organization_id,
            access_token_hash: &access.hash,
            refresh_token_hash: &refresh.hash,
            csrf_token_hash: &csrf.hash,
            family_id: &family_id,
            now,
            access_expires_at,
            refresh_expires_at,
            user_agent_hash: ua.as_deref(),
            ip_hash: None,
        })
        .await
        .map_err(HubError::Store)?;
    Ok(SessionTokens {
        session_id,
        access,
        refresh,
        csrf,
        access_expires_at,
        refresh_expires_at,
    })
}

#[derive(Debug, Serialize)]
pub(super) struct AuthBody {
    access_token:            String,
    token_type:              &'static str,
    access_expires_at:       i64,
    refresh_expires_at:      i64,
    csrf_token:              String,
    user:                    JsonValue,
    current_organization_id: String,
}
fn auth_response(
    state: &AppState,
    status: StatusCode,
    user: nagisalake_hub_store::User,
    organization_id: String,
    tokens: SessionTokens,
) -> Response {
    let mut response = (
        status,
        Json(AuthBody {
            access_token:            tokens.access.plaintext,
            token_type:              "Bearer",
            access_expires_at:       tokens.access_expires_at,
            refresh_expires_at:      tokens.refresh_expires_at,
            csrf_token:              tokens.csrf.plaintext.clone(),
            user:                    public_user(&user),
            current_organization_id: organization_id,
        }),
    )
        .into_response();
    append_cookie(
        &mut response,
        refresh_cookie(
            state,
            &tokens.refresh.plaintext,
            state.config.browser.refresh_ttl_seconds,
            true,
        ),
    );
    append_cookie(
        &mut response,
        csrf_cookie(
            state,
            &tokens.csrf.plaintext,
            state.config.browser.refresh_ttl_seconds,
        ),
    );
    response
}
fn public_user(user: &nagisalake_hub_store::User) -> JsonValue {
    json!({"id":user.id,"email":user.email,"status":user.status,"email_verified":user.email_verified_at.is_some(),"created_at":user.created_at})
}
fn refresh_cookie(state: &AppState, value: &str, max_age: i64, http_only: bool) -> String {
    let mut value =
        format!("{REFRESH_COOKIE}={value}; Path=/api/v1/auth; Max-Age={max_age}; SameSite=Lax");
    if http_only {
        value.push_str("; HttpOnly")
    }
    if state.config.browser.cookie_secure {
        value.push_str("; Secure")
    }
    value
}
fn csrf_cookie(state: &AppState, value: &str, max_age: i64) -> String {
    let mut value = format!("{CSRF_COOKIE}={value}; Path=/; Max-Age={max_age}; SameSite=Lax");
    if state.config.browser.cookie_secure {
        value.push_str("; Secure")
    }
    value
}
fn clear_auth_cookies(state: &AppState, status: StatusCode) -> Response {
    let mut response = status.into_response();
    append_cookie(&mut response, refresh_cookie(state, "", 0, true));
    append_cookie(&mut response, csrf_cookie(state, "", 0));
    response
}
fn append_cookie(response: &mut Response, value: String) {
    if let Ok(value) = HeaderValue::from_str(&value) {
        response.headers_mut().append(SET_COOKIE, value);
    }
}
fn cookies(headers: &HeaderMap) -> std::collections::HashMap<&str, &str> {
    headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .into_iter()
        .flat_map(|value| value.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .collect()
}
fn verify_origin(state: &AppState, headers: &HeaderMap) -> Result<(), HubError> {
    if state.config.browser.allowed_origins.is_empty() {
        return Ok(());
    }
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| HubError::Forbidden("Origin header is required".into()))?;
    if state
        .config
        .browser
        .allowed_origins
        .iter()
        .any(|allowed| allowed == origin)
    {
        return Ok(());
    }
    // A console compiled into this binary is served from the Hub's own origin,
    // so it would otherwise need to be listed in allowed_origins. Requests whose
    // Origin authority equals the request's Host are same-origin by definition
    // and carry no CSRF risk, so accept them without extra configuration.
    if is_same_origin(origin, headers) {
        return Ok(());
    }
    Err(HubError::Forbidden("Origin is not allowed".into()))
}

/// Compares the authority (`host[:port]`) of `origin` against the request Host.
/// Scheme is deliberately excluded: with TLS terminated at a proxy the Hub sees
/// `http` while the browser sends an `https` Origin for the same site.
pub(super) fn is_same_origin(origin: &str, headers: &HeaderMap) -> bool {
    let Some(host) = headers.get("host").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let authority = origin
        .split_once("://")
        .map(|(_scheme, rest)| rest)
        .unwrap_or(origin);
    !authority.is_empty() && authority.eq_ignore_ascii_case(host)
}
