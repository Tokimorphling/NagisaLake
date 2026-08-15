use super::*;

/// Same-origin browser requests are accepted without listing the Hub's own
/// origin in allowed_origins, which is what an embedded console needs.
#[test]
fn same_origin_requests_satisfy_the_origin_check() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "host",
        axum::http::HeaderValue::from_static("hub.example.com"),
    );

    // TLS terminated at a proxy: browser sends https, Hub sees http.
    assert!(product_api::is_same_origin(
        "https://hub.example.com",
        &headers
    ));
    assert!(product_api::is_same_origin(
        "http://hub.example.com",
        &headers
    ));
    // A different site must still be rejected.
    assert!(!product_api::is_same_origin(
        "https://evil.example.com",
        &headers
    ));
    // Port mismatch is a different origin.
    assert!(!product_api::is_same_origin(
        "https://hub.example.com:8443",
        &headers
    ));
    // A null/opaque origin must not pass.
    assert!(!product_api::is_same_origin("null", &headers));
}

#[cfg(feature = "embed-web")]
mod console {
    use super::*;

    #[tokio::test]
    async fn embedded_console_serves_the_shell_and_spa_deep_links() {
        let address = spawn_router().await;
        let client = reqwest::Client::new();

        // Root serves the shell as HTML that must revalidate.
        let response = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .starts_with("text/html")
        );
        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "no-cache",
            "index.html must revalidate or clients pin a stale asset graph"
        );
        let shell = response.text().await.unwrap();
        assert!(shell.contains("<div id=\"root\">"));

        // A client-side route must resolve to the same shell on hard reload.
        let deep = client
            .get(format!("http://{address}/jobs/abc-123"))
            .send()
            .await
            .unwrap();
        assert_eq!(deep.status(), reqwest::StatusCode::OK);
        assert!(deep.text().await.unwrap().contains("<div id=\"root\">"));
    }

    /// A missing hashed asset must 404 rather than resolve to HTML, which the
    /// browser would reject for a script or style request under nosniff.
    #[tokio::test]
    async fn missing_hashed_assets_do_not_fall_back_to_html() {
        let address = spawn_router().await;
        let response = reqwest::get(format!("http://{address}/assets/not-real.js"))
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(
            content_type.starts_with("application/json"),
            "{content_type}"
        );
    }

    /// Hashed assets are immutable and must answer conditional requests with
    /// 304 so reloads do not re-download the bundle.
    #[tokio::test]
    async fn hashed_assets_are_immutable_and_honour_etags() {
        let address = spawn_router().await;
        let client = reqwest::Client::new();

        let shell = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let asset = shell
            .split("src=\"/")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("shell references a hashed script");
        assert!(
            asset.starts_with("assets/"),
            "unexpected asset path {asset}"
        );

        let response = client
            .get(format!("http://{address}/{asset}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .contains("javascript"),
            "hashed script must keep a JavaScript content type"
        );
        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "public, max-age=31536000, immutable"
        );
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .expect("asset carries an ETag")
            .to_owned();
        assert!(!response.bytes().await.unwrap().is_empty());

        let cached = client
            .get(format!("http://{address}/{asset}"))
            .header("if-none-match", &etag)
            .send()
            .await
            .unwrap();
        assert_eq!(cached.status(), reqwest::StatusCode::NOT_MODIFIED);
        assert!(cached.bytes().await.unwrap().is_empty());
    }

    /// The console must not shadow the API, and writes to unknown non-API
    /// paths must not be answered with the shell.
    #[tokio::test]
    async fn console_never_shadows_api_routes() {
        let address = spawn_router().await;
        let client = reqwest::Client::new();

        // A real API route still answers from the API, not the shell.
        let settings = client
            .get(format!("http://{address}/api/v1/settings/public"))
            .send()
            .await
            .unwrap();
        assert_eq!(settings.status(), reqwest::StatusCode::OK);
        let body: JsonValue = settings.json().await.unwrap();
        assert!(body["authentication"].is_array());

        let health = client
            .get(format!("http://{address}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), reqwest::StatusCode::OK);

        // POST to an unknown non-API path must not return HTML.
        let posted = client
            .post(format!("http://{address}/some/page"))
            .send()
            .await
            .unwrap();
        assert_eq!(posted.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
    }
}

/// The exposure guards exist to stop a deployment that is unsafe the moment
/// it is reachable off-host. Each must fire, and each must be escapable
/// deliberately, or operators will work around them by other means.
#[test]
fn exposure_guards_fire_only_when_reachable_off_host() {
    let off_host = |mut config: HubConfig| {
        config.server.listen = "0.0.0.0:9091".parse().unwrap();
        config
    };
    let loopback = |mut config: HubConfig| {
        config.server.listen = "127.0.0.1:9091".parse().unwrap();
        config
    };

    // Plain-HTTP session cookies on a reachable address: refused.
    let mut insecure = off_host(config());
    insecure.browser.cookie_secure = false;
    insecure.rate_limit.enabled = true;
    let error = insecure.validate().expect_err("should refuse to start");
    assert!(
        error.to_string().contains("cookie_secure"),
        "unexpected error: {error}"
    );

    // The same settings on loopback are fine: nothing leaves the machine.
    assert!(loopback(insecure.clone()).validate().is_ok());

    // And the trusted-LAN escape hatch works, since that is a real setup.
    let mut acknowledged = insecure.clone();
    acknowledged.browser.allow_insecure_cookies = true;
    assert!(acknowledged.validate().is_ok());

    // HTTPS plus rate limiting needs no acknowledgement.
    let mut production = off_host(config());
    production.browser.cookie_secure = true;
    production.rate_limit.enabled = true;
    assert!(production.validate().is_ok());

    // Open registration with throttling switched off: refused.
    let mut unthrottled = off_host(config());
    unthrottled.browser.cookie_secure = true;
    unthrottled.browser.registration_enabled = true;
    unthrottled.rate_limit.enabled = false;
    let error = unthrottled.validate().expect_err("should refuse to start");
    assert!(
        error.to_string().contains("rate_limit"),
        "unexpected error: {error}"
    );
    // Closing registration is the other way out.
    let mut closed = unthrottled.clone();
    closed.browser.registration_enabled = false;
    assert!(closed.validate().is_ok());
}

/// The legacy static tokens bypass every account check, so shipping the
/// documented example values on a reachable address is a credential leak.
#[test]
fn example_legacy_tokens_are_refused_off_host() {
    let mut config = config();
    config.server.listen = "0.0.0.0:9091".parse().unwrap();
    config.browser.cookie_secure = true;
    config.rate_limit.enabled = true;
    config.auth.consumer_token = Some("development-api-token".into());

    let error = config
        .validate()
        .expect_err("should refuse an example token");
    assert!(
        error.to_string().contains("example value"),
        "unexpected error: {error}"
    );

    // A real secret passes.
    config.auth.consumer_token = Some("a-real-secret-value".into());
    assert!(config.validate().is_ok());

    // On loopback the example values are exactly what the docs tell people
    // to use, so they must keep working.
    config.server.listen = "127.0.0.1:9091".parse().unwrap();
    config.auth.consumer_token = Some("development-api-token".into());
    assert!(config.validate().is_ok());
}

/// A provider whose secret is missing must fail startup rather than render a
/// sign-in button that returns 500 when a user clicks it.
#[test]
fn oauth_configuration_is_validated_at_startup() {
    let mut config = config();
    config.oauth = Some(crate::oauth::OauthConfig {
        public_url: "https://hub.example".into(),
        providers:  std::collections::BTreeMap::from([(
            "google".into(),
            crate::oauth::ProviderConfig {
                kind:                  crate::oauth::ProviderKind::Google,
                client_id:             "client".into(),
                client_secret_env:     "NAGISALAKE_TEST_ABSENT_SECRET".into(),
                authorize_url:         None,
                token_url:             None,
                userinfo_url:          None,
                scopes:                None,
                allowed_email_domains: Vec::new(),
            },
        )]),
    });
    let error = config
        .validate()
        .expect_err("a missing secret must fail startup");
    assert!(
        error.to_string().contains("NAGISALAKE_TEST_ABSENT_SECRET"),
        "the error should name the variable: {error}"
    );

    // A relative public_url cannot build a redirect URI the provider accepts.
    config.oauth.as_mut().unwrap().public_url = "hub.example".into();
    assert!(config.validate().is_err());
}

#[test]
fn example_hub_config_is_parseable() {
    let config: HubConfig =
        toml::from_str(include_str!("../../../../../examples/nagisalake-hub.toml")).unwrap();
    assert_eq!(config.server.listen.port(), 9091);
    assert!(config.object_store.is_some());
}
