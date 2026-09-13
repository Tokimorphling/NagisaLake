use super::*;
use nagisalake_agent::{Completion, Execution};
use nagisalake_hub_auth::{PrincipalKind, Role};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

#[derive(Clone)]
struct Waiting;
impl Backend for Waiting {
    async fn execute(&self, _execution: Execution) -> Result<Completion, AgentError> {
        std::future::pending().await
    }
}
fn principal(organization_id: &str, user: &str) -> Principal {
    Principal {
        organization_id: organization_id.into(),
        actor_id:        format!("session-{user}"),
        user_id:         Some(user.into()),
        kind:            PrincipalKind::BrowserSession,
        role:            Role::Member,
        scopes:          BTreeSet::new(),
    }
}
fn request() -> RunRequest {
    RunRequest {
        skill:   "rewrite".into(),
        input:   "hello".into(),
        options: BTreeMap::new(),
    }
}

#[tokio::test]
async fn cancellation_checks_owner_and_organization_and_per_org_admission_is_bounded() {
    let service = AgentService::new(
        Waiting,
        vec![Skill {
            id:      "rewrite".into(),
            version: "1".into(),
        }],
        RuntimeConfig::default(),
    )
    .unwrap();
    let state = AgentState::new(service, 1, 120);
    let a = principal("org-a", "user-a");
    let run = state.start(&a, request(), None).await.unwrap();
    assert!(matches!(
        state.start(&a, request(), None).await,
        Err(HubError::QuotaExceeded(_))
    ));
    assert!(!state.cancel(&principal("org-b", "user-a"), run.id()));
    assert!(!state.cancel(&principal("org-a", "user-b"), run.id()));
    let mut rotated = a.clone();
    rotated.actor_id = "new-session".into();
    assert!(state.cancel(&rotated, run.id()));
    assert!(matches!(
        &*run.completion().wait().await.unwrap(),
        RunEvent::Cancelled { .. }
    ));
}

#[test]
fn config_is_optional_and_empty_allowlist_does_not_authorize_discovered_skills() {
    let config: AgentConfig = toml::from_str("base_url = 'http://127.0.0.1:4096'").unwrap();
    assert!(config.allowed_skills.is_empty());
    assert_eq!(config.runtime.event_buffer, 256);
    assert!(
        config
            .build()
            .unwrap()
            .service
            .validate_request(&request())
            .is_err()
    );
}

async fn spawn(app: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}

#[tokio::test]
async fn http_runs_require_auth_and_stream_a_stable_protocol() {
    use axum::{
        Json, Router,
        http::StatusCode,
        routing::{get, post},
    };
    use serde_json::{Value, json};
    let mock = Router::new()
        .route("/event", get(|| async {StatusCode::SERVICE_UNAVAILABLE}))
        .route("/session", post(|Json(_): Json<Value>| async {Json(json!({"id":"ses_http"}))}))
        .route("/session/{id}/prompt_async", post(|Json(_): Json<Value>| async {StatusCode::NO_CONTENT}))
        .route("/session/{id}/message", get(|| async {Json(json!([
            {"info":{"id":"msg_1","role":"assistant","time":{"created":1,"completed":2},"finish":"tool-calls"},"parts":[
                {"type":"tool","tool":"skill","callID":"call_1","state":{"status":"completed","input":{"name":"rewrite"},"output":"DO NOT EXPOSE"}}]},
            {"info":{"id":"msg_2","role":"assistant","time":{"created":3,"completed":4},"finish":"stop"},"parts":[
                {"type":"text","id":"part_2","messageID":"msg_2","text":"improved text"}]}
        ]))}))
        .route("/session/{id}", axum::routing::delete(|| async {Json(true)}));
    let (upstream, upstream_task) = spawn(mock).await;
    let config: crate::HubConfig = toml::from_str(&format!(
        r#"[auth]
worker_token = 'worker-test'
consumer_token = 'consumer-test'
[opencode]
base_url = '{upstream}'
allowed_skills = ['rewrite']
poll_interval_ms = 50
"#
    ))
    .unwrap();
    let (base, task) = spawn(crate::router(config).await.unwrap()).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("{base}/api/v1/agent/runs/stream");
    let unauthorized = client.post(&url).json(&request()).send().await.unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let denied = client
        .post(&url)
        .bearer_auth("consumer-test")
        .json(&json!({"skill":"shell","input":"hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    let response = client
        .post(&url)
        .bearer_auth("consumer-test")
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-accel-buffering"], "no");
    let data = response.text().await.unwrap();
    for event in ["started", "stage_finished", "completed"] {
        assert!(data.contains(&format!("event: {event}")), "{data}");
    }
    assert!(!data.contains("DO NOT EXPOSE"));
    assert!(!data.contains("ses_http"));
    let response = client
        .post(format!("{base}/v1/enhance"))
        .bearer_auth("consumer-test")
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["text"], "improved text");
    task.abort();
    upstream_task.abort();
}
