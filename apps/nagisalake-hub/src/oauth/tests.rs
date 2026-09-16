use super::{
    config::{OauthConfig, OauthError, Provider, ProviderConfig, ProviderKind},
    exchange::{
        exchange_and_fetch_identity, identity_from_id_token, linuxdo_identity_from_userinfo,
        parse_token_response,
    },
    pkce::{authorize_url, generate_pkce, generate_state, sanitize_redirect_path},
};
use nagisalake_hub_auth::hash_secret;
use std::collections::BTreeMap;

#[test]
fn redirect_paths_cannot_leave_the_site() {
    assert_eq!(sanitize_redirect_path(Some("/jobs")), "/jobs");
    assert_eq!(sanitize_redirect_path(Some("/jobs?a=1")), "/jobs?a=1");
    assert_eq!(sanitize_redirect_path(None), "/");
    assert_eq!(sanitize_redirect_path(Some("")), "/");
    // A protocol-relative URL reads as another host to the browser.
    assert_eq!(sanitize_redirect_path(Some("//evil.example")), "/");
    assert_eq!(sanitize_redirect_path(Some("https://evil.example")), "/");
    assert_eq!(sanitize_redirect_path(Some("jobs")), "/");
    // Backslashes are normalised to slashes by some browsers.
    assert_eq!(sanitize_redirect_path(Some("/\\evil.example")), "/");
}

#[test]
fn pkce_challenge_is_base64url_of_the_verifier_digest() {
    let pkce = generate_pkce();
    assert!(pkce.verifier.len() >= 43, "verifier must be long enough");
    assert!(!pkce.challenge.contains('='), "must be unpadded base64url");
    assert!(!pkce.challenge.contains('+') && !pkce.challenge.contains('/'));

    // Recompute independently.
    let digest = hash_secret(&pkce.verifier);
    let raw = data_encoding::HEXLOWER.decode(digest.as_bytes()).unwrap();
    assert_eq!(pkce.challenge, data_encoding::BASE64URL_NOPAD.encode(&raw));

    // Two calls must not collide.
    assert_ne!(generate_pkce().verifier, generate_pkce().verifier);
    assert_ne!(generate_state(), generate_state());
}

#[test]
fn google_identity_comes_from_the_id_token() {
    // header.payload.signature; only the payload is read.
    let payload = serde_json::json!({
        "sub": "1234567890",
        "email": "user@example.com",
        "email_verified": true,
        "name": "Example User"
    });
    let encoded = data_encoding::BASE64URL_NOPAD.encode(payload.to_string().as_bytes());
    let token = format!("header.{encoded}.signature");

    let identity = identity_from_id_token(&token).expect("payload should parse");
    assert_eq!(identity.subject, "1234567890");
    assert_eq!(identity.email, "user@example.com");
    assert!(identity.email_verified);
    assert_eq!(identity.display_name.as_deref(), Some("Example User"));
}

#[test]
fn an_unverified_or_malformed_id_token_does_not_claim_verification() {
    let payload = serde_json::json!({"sub": "1", "email": "a@b.c"});
    let encoded = data_encoding::BASE64URL_NOPAD.encode(payload.to_string().as_bytes());
    let identity = identity_from_id_token(&format!("h.{encoded}.s")).unwrap();
    assert!(
        !identity.email_verified,
        "a missing email_verified claim must not be treated as verified"
    );

    // The string form some providers send.
    let payload = serde_json::json!({"sub": "1", "email": "a@b.c", "email_verified": "true"});
    let encoded = data_encoding::BASE64URL_NOPAD.encode(payload.to_string().as_bytes());
    assert!(
        identity_from_id_token(&format!("h.{encoded}.s"))
            .unwrap()
            .email_verified
    );

    // Missing sub or email yields nothing rather than a partial identity.
    for payload in [
        serde_json::json!({"email": "a@b.c"}),
        serde_json::json!({"sub": "1"}),
    ] {
        let encoded = data_encoding::BASE64URL_NOPAD.encode(payload.to_string().as_bytes());
        assert!(identity_from_id_token(&format!("h.{encoded}.s")).is_none());
    }
    assert!(identity_from_id_token("not-a-token").is_none());
    assert!(identity_from_id_token("h.!!!.s").is_none());
}

#[test]
fn domain_restriction_matches_only_the_listed_domains() {
    let mut provider = Provider {
        name:                  "google".into(),
        kind:                  ProviderKind::Google,
        client_id:             "id".into(),
        client_secret:         "secret".into(),
        authorize_url:         "https://a".into(),
        token_url:             "https://t".into(),
        userinfo_url:          "https://u".into(),
        scopes:                vec!["email".into()],
        allowed_email_domains: Vec::new(),
    };
    // Empty means unrestricted.
    assert!(provider.allows_email("anyone@gmail.com"));

    provider.allowed_email_domains = vec!["example.com".into()];
    assert!(provider.allows_email("user@example.com"));
    assert!(
        provider.allows_email("USER@EXAMPLE.COM"),
        "case-insensitive"
    );
    assert!(!provider.allows_email("user@other.com"));
    // A lookalike must not pass.
    assert!(!provider.allows_email("user@notexample.com"));
    assert!(!provider.allows_email("malformed"));
}

#[test]
fn authorize_url_carries_state_and_challenge() {
    let provider = Provider {
        name:                  "google".into(),
        kind:                  ProviderKind::Google,
        client_id:             "client id".into(),
        client_secret:         "secret".into(),
        authorize_url:         "https://accounts.example/auth".into(),
        token_url:             "https://t".into(),
        userinfo_url:          "https://u".into(),
        scopes:                vec!["openid".into(), "email".into()],
        allowed_email_domains: Vec::new(),
    };
    let url = authorize_url(
        &provider,
        "https://hub.example/api/v1/auth/oauth/google/callback",
        "state-value",
        "challenge-value",
    );
    assert!(url.starts_with("https://accounts.example/auth?"));
    assert!(url.contains("state=state-value"));
    assert!(url.contains("code_challenge=challenge-value"));
    assert!(url.contains("code_challenge_method=S256"));
    // Spaces and separators must be encoded.
    assert!(url.contains("client_id=client%20id"));
    assert!(url.contains("scope=openid%20email"));
    assert!(url.contains("redirect_uri=https%3A%2F%2Fhub.example%2F"));
    // The secret must never appear in a URL the browser sees.
    assert!(!url.contains("secret"));
}

#[test]
fn resolving_a_provider_requires_its_secret() {
    let config = OauthConfig {
        public_url: "https://hub.example".into(),
        providers:  BTreeMap::from([("google".into(), ProviderConfig {
            kind:                  ProviderKind::Google,
            client_id:             "client".into(),
            client_secret_env:     "NAGISALAKE_TEST_OAUTH_SECRET_MISSING".into(),
            authorize_url:         None,
            token_url:             None,
            userinfo_url:          None,
            scopes:                None,
            allowed_email_domains: Vec::new(),
        })]),
    };
    // A missing secret fails startup rather than producing a broken button.
    assert!(matches!(
        config.resolve(),
        Err(OauthError::InvalidConfig(_))
    ));

    // SAFETY: single-threaded test, and the variable is unique to it.
    unsafe { std::env::set_var("NAGISALAKE_TEST_OAUTH_SECRET_MISSING", "s3cret") };
    let resolved = config
        .resolve()
        .expect("should resolve once the secret exists");
    let provider = &resolved["google"];
    assert_eq!(provider.client_secret, "s3cret");
    // Built-in endpoints are filled in.
    assert_eq!(provider.token_url, "https://oauth2.googleapis.com/token");
    assert!(provider.scopes.contains(&"email".to_string()));
    unsafe { std::env::remove_var("NAGISALAKE_TEST_OAUTH_SECRET_MISSING") };
}

#[test]
fn public_url_must_be_absolute() {
    for public_url in ["", "hub.example", "/hub"] {
        let config = OauthConfig {
            public_url: public_url.into(),
            providers:  BTreeMap::new(),
        };
        assert!(
            matches!(config.resolve(), Err(OauthError::InvalidConfig(_))),
            "{public_url:?} should be rejected"
        );
    }
    let config = OauthConfig {
        public_url: "https://hub.example/".into(),
        providers:  BTreeMap::new(),
    };
    assert!(config.resolve().is_ok());
    // The trailing slash must not double up.
    assert_eq!(
        config.redirect_uri("google"),
        "https://hub.example/api/v1/auth/oauth/google/callback"
    );
}

#[test]
fn github_needs_the_email_scope_by_default() {
    // Without user:email the verification status is unavailable, so linking
    // to an existing account could never be allowed.
    assert!(
        ProviderKind::Github
            .default_scopes()
            .contains(&"user:email".to_string())
    );
}

#[test]
fn linuxdo_defaults_match_connect() {
    assert_eq!(
        ProviderKind::Linuxdo.authorize_url(),
        "https://connect.linux.do/oauth2/authorize"
    );
    assert_eq!(
        ProviderKind::Linuxdo.token_url(),
        "https://connect.linux.do/oauth2/token"
    );
    assert_eq!(
        ProviderKind::Linuxdo.userinfo_url(),
        "https://connect.linux.do/api/user"
    );
    assert_eq!(ProviderKind::Linuxdo.default_scopes(), vec!["user"]);
}

#[test]
fn linuxdo_userinfo_uses_a_subject_scoped_synthetic_email() {
    let identity = linuxdo_identity_from_userinfo(&serde_json::json!({
        "id": 123,
        "username": "alice",
        "name": "Alice"
    }))
    .unwrap();
    assert_eq!(identity.subject, "123");
    assert_eq!(identity.email, "linuxdo-123@linuxdo-connect.invalid");
    assert!(identity.email_verified);
    assert_eq!(identity.display_name.as_deref(), Some("Alice"));
}

#[test]
fn linuxdo_userinfo_rejects_an_unsafe_subject() {
    for claims in [
        serde_json::json!({}),
        serde_json::json!({"id": "123@evil.example"}),
        serde_json::json!({"id": "a".repeat(57)}),
    ] {
        assert!(linuxdo_identity_from_userinfo(&claims).is_err());
    }
}

#[test]
fn token_response_accepts_json_and_form_encoding() {
    for body in [
        &b"{\"access_token\":\"json-token\"}"[..],
        &b"access_token=form-token&token_type=bearer"[..],
    ] {
        assert!(parse_token_response(body).unwrap().access_token.is_some());
    }
}

#[tokio::test]
async fn linuxdo_exchange_accepts_form_token_and_numeric_user_id() {
    use axum::{
        Json, Router,
        body::Bytes,
        http::{HeaderMap, header::CONTENT_TYPE},
        response::IntoResponse,
        routing::{get, post},
    };

    async fn token(body: Bytes) -> impl IntoResponse {
        let form: BTreeMap<String, String> = serde_urlencoded::from_bytes(&body).unwrap();
        assert_eq!(form["client_id"], "linuxdo-client");
        assert_eq!(form["client_secret"], "linuxdo-secret");
        assert_eq!(form["code_verifier"], "pkce-verifier");
        (
            [(CONTENT_TYPE, "application/x-www-form-urlencoded")],
            "access_token=linuxdo-access&token_type=bearer",
        )
    }

    async fn user(headers: HeaderMap) -> Json<serde_json::Value> {
        assert_eq!(headers["authorization"], "Bearer linuxdo-access");
        Json(serde_json::json!({
            "id": 123,
            "username": "alice",
            "name": "Alice"
        }))
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/token", post(token))
                .route("/user", get(user)),
        )
        .await
    });
    let provider = Provider {
        name:                  "linuxdo".into(),
        kind:                  ProviderKind::Linuxdo,
        client_id:             "linuxdo-client".into(),
        client_secret:         "linuxdo-secret".into(),
        authorize_url:         format!("http://{address}/authorize"),
        token_url:             format!("http://{address}/token"),
        userinfo_url:          format!("http://{address}/user"),
        scopes:                vec!["user".into()],
        allowed_email_domains: Vec::new(),
    };

    let identity = exchange_and_fetch_identity(
        &reqwest::Client::new(),
        &provider,
        "https://hub.example/api/v1/auth/oauth/linuxdo/callback",
        "authorization-code",
        "pkce-verifier",
    )
    .await
    .unwrap();
    assert_eq!(identity.subject, "123");
    assert_eq!(identity.email, "linuxdo-123@linuxdo-connect.invalid");
    server.abort();
}
