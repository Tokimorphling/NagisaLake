use super::*;
use crate::{AgentError, AgentService, RunEvent, RunRequest, RuntimeConfig, Skill};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use service_async::Service;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, broadcast};

mod streaming;

#[test]
fn sse_handles_chunking_unicode_crlf_and_multiple_data_lines() {
    let mut decoder = sse::Decoder::new(1024);
    let bytes = "data: {\"type\":\"session.idle\",\r\ndata: \
                 \"properties\":{\"sessionID\":\"ses_你\"}}\r\n\r\n"
        .as_bytes();
    let mut events = Vec::new();
    for byte in bytes {
        events.extend(decoder.push(&[*byte]).unwrap());
    }
    assert_eq!(events.len(), 1);
    assert_eq!(
        wire::WireEvent::parse(&events[0]).unwrap().session_id(),
        Some("ses_你")
    );
    assert_eq!(decoder.push(b"data: 1\r\rdata: 2\n\n").unwrap(), vec![
        b"1".to_vec(),
        b"2".to_vec()
    ]);
    assert!(sse::Decoder::new(8).push(b"data: aaaaaaaaa").is_err());
}

#[test]
fn wire_accepts_direct_and_wrapped_events_without_exposing_raw_data() {
    for data in [br#"{"type":"message.part.updated","properties":{"part":{"sessionID":"ses_one"}}}"#.as_slice(),
        br#"{"payload":{"type":"message.part.updated","properties":{"part":{"sessionID":"ses_one"}}}}"#.as_slice()] {
        assert_eq!(wire::WireEvent::parse(data).unwrap().session_id(), Some("ses_one"));
    }
    let error = wire::WireEvent::parse(b"secret value not JSON").unwrap_err();
    assert!(!error.to_string().contains("secret"));
}

#[test]
fn persisted_user_summaries_may_be_objects_not_booleans() {
    let message: wire::Message = serde_json::from_value(json!({
        "info":{"id":"msg_user","role":"user","time":{"created":1},"summary":{"title":"A prompt"}},
        "parts":[]
    }))
    .unwrap();
    assert!(!message.info.summary);
}

#[derive(Default)]
struct Mock {
    created:       AtomicUsize,
    streams:       AtomicUsize,
    prompts:       AtomicUsize,
    polls:         Mutex<HashMap<String, usize>>,
    sessions:      Mutex<Vec<Value>>,
    prompt_bodies: Mutex<Vec<Value>>,
    aborted:       Mutex<Vec<String>>,
    deleted:       Mutex<Vec<String>>,
    notify:        Notify,
    hang_prompt:   bool,
    fail_prompt:   bool,
    events:        Mutex<Option<broadcast::Sender<String>>>,
    slow_delete:   AtomicBool,
}

async fn spawn_mock(
    hang_prompt: bool,
    fail_prompt: bool,
) -> (OpenCodeConfig, Arc<Mock>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(Mock {
        hang_prompt,
        fail_prompt,
        ..Mock::default()
    });
    let routes = Router::new()
        .route("/event", get(events))
        .route("/session", post(create))
        .route("/session/{id}/prompt_async", post(prompt))
        .route("/session/{id}/message", get(messages))
        .route("/session/{id}/abort", post(abort))
        .route("/session/{id}", axum::routing::delete(remove))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let config = OpenCodeConfig {
        base_url: format!("http://{address}"),
        poll_interval_ms: 50,
        request_timeout_seconds: 2,
        ..OpenCodeConfig::default()
    };
    (config, state, task)
}

async fn events(State(state): State<Arc<Mock>>) -> Response {
    state.streams.fetch_add(1, Ordering::Relaxed);
    if let Some(sender) = state.events.lock().unwrap().clone() {
        return streaming::response(sender);
    }
    // Deliberately broken upstream SSE: runs must still finish using polling.
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "sensitive proxy diagnostic",
    )
        .into_response()
}
async fn create(State(state): State<Arc<Mock>>, Json(body): Json<Value>) -> Json<Value> {
    state.sessions.lock().unwrap().push(body);
    let id = state.created.fetch_add(1, Ordering::Relaxed);
    Json(json!({"id":format!("ses_{id}")}))
}
async fn prompt(
    State(state): State<Arc<Mock>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    assert!(
        body["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("rewrite")
    );
    assert!(body.get("tools").is_none()); // do not overwrite session deny rules
    state.prompt_bodies.lock().unwrap().push(body);
    state.prompts.fetch_add(1, Ordering::Relaxed);
    state.notify.notify_waiters();
    if state.hang_prompt {
        return std::future::pending().await;
    }
    if state.fail_prompt {
        return (
            StatusCode::UNAUTHORIZED,
            "secret provider key and file path",
        )
            .into_response();
    }
    if let Some(sender) = state.events.lock().unwrap().clone() {
        streaming::publish(&sender, &id);
    }
    StatusCode::NO_CONTENT.into_response()
}
async fn messages(State(state): State<Arc<Mock>>, Path(id): Path<String>) -> Json<Value> {
    let mut polls = state.polls.lock().unwrap();
    let count = polls.entry(id.clone()).or_default();
    *count += 1;
    let tool = json!({"info":{"id":"msg_1","role":"assistant","time":{"created":1,"completed":2},"finish":"tool-calls"},
        "parts":[{"id":"part_skill","messageID":"msg_1","type":"tool","callID":"call_1","tool":"skill",
            "state":{"status":"completed","input":{"name":"rewrite"},"output":"SECRET tool output"}},
            {"id":"part_reason","messageID":"msg_1","type":"reasoning","text":"SECRET reasoning"}]});
    let completion_poll = if state.events.lock().unwrap().is_some() {
        8
    } else {
        2
    };
    if *count < completion_poll {
        return Json(json!([tool]));
    }
    Json(
        json!([tool, {"info":{"id":"msg_2","role":"assistant","time":{"created":3,"completed":4},"finish":"stop"},
        "parts":[{"id":"part_text","messageID":"msg_2","type":"text","text":format!("result:{id}")}]}]),
    )
}
async fn abort(State(state): State<Arc<Mock>>, Path(id): Path<String>) -> Json<bool> {
    state.aborted.lock().unwrap().push(id);
    state.notify.notify_waiters();
    Json(true)
}
async fn remove(State(state): State<Arc<Mock>>, Path(id): Path<String>) -> Json<bool> {
    state.deleted.lock().unwrap().push(id);
    state.notify.notify_waiters();
    if state.slow_delete.load(Ordering::Relaxed) {
        return std::future::pending().await;
    }
    Json(true)
}

fn service(config: OpenCodeConfig) -> AgentService<OpenCode> {
    AgentService::new(
        OpenCode::new(config).unwrap(),
        vec![Skill {
            id:      "rewrite".into(),
            version: "1".into(),
        }],
        RuntimeConfig::default(),
    )
    .unwrap()
}
fn request() -> RunRequest {
    RunRequest {
        skill:   "rewrite".into(),
        input:   "a simple sentence".into(),
        options: HashMap::new().into_iter().collect(),
    }
}

#[tokio::test]
async fn configured_model_ids_are_sent_using_opencode_wire_names() {
    let (mut config, state, task) = spawn_mock(false, false).await;
    config.model = Some(
        toml::from_str::<ModelRef>(
            r#"
provider_id = "opencode"
model_id = "mimo-v2.5-free"
"#,
        )
        .unwrap(),
    );
    let service = service(config);
    let run = service.call(request()).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(3), run.completion().wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&*outcome, RunEvent::Completed { .. }));
    let prompts = state.prompt_bodies.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(
        prompts[0]["model"],
        json!({
            "providerID": "opencode", "modelID": "mimo-v2.5-free"
        })
    );
    task.abort();
}

#[tokio::test]
async fn polling_recovers_after_sse_failure_and_does_not_finish_at_tool_calls() {
    let (config, state, task) = spawn_mock(false, false).await;
    let service = service(config);
    let mut run = service.call(request()).await.unwrap();
    let mut all = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event) = run.recv().await {
            all.push(event);
        }
    })
    .await
    .unwrap();
    assert!(matches!(all.last(), Some(RunEvent::Completed { text, .. }) if text == "result:ses_0"));
    assert_eq!(
        all.iter()
            .filter(|event| matches!(event, RunEvent::StageFinished { .. }))
            .count(),
        1
    );
    assert!(!serde_json::to_string(&all).unwrap().contains("SECRET"));
    let sessions = state.sessions.lock().unwrap();
    assert_eq!(
        sessions[0]["permission"],
        json!([
        {"permission":"*","pattern":"*","action":"deny"},
        {"permission":"skill","pattern":"rewrite","action":"allow"}])
    );
    task.abort();
}

#[tokio::test]
async fn concurrent_runs_get_isolated_sessions_and_share_router_initialization() {
    let (config, state, task) = spawn_mock(false, false).await;
    let service = service(config);
    let (a, b) = tokio::join!(service.call(request()), service.call(request()));
    let (a, b) = (a.unwrap(), b.unwrap());
    let (mut a_completion, mut b_completion) = (a.completion(), b.completion());
    let (a_done, b_done) = tokio::join!(a_completion.wait(), b_completion.wait());
    let a_done = a_done.unwrap();
    let b_done = b_done.unwrap();
    assert!(matches!(&*a_done, RunEvent::Completed { .. }));
    assert!(matches!(&*b_done, RunEvent::Completed { .. }));
    assert_ne!(a_done, b_done);
    assert_eq!(state.created.load(Ordering::Relaxed), 2);
    assert_eq!(state.streams.load(Ordering::Relaxed), 1);
    task.abort();
}

async fn wait_for(state: &Mock, predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let notified = state.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if predicate() {
                break;
            }
            notified.await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dropping_during_prompt_async_aborts_and_cleans_up_the_session() {
    let (config, state, task) = spawn_mock(true, false).await;
    let service = service(config);
    let run = service.call(request()).await.unwrap();
    wait_for(&state, || state.prompts.load(Ordering::Relaxed) > 0).await;
    drop(run);
    wait_for(&state, || !state.deleted.lock().unwrap().is_empty()).await;
    assert_eq!(&*state.aborted.lock().unwrap(), &["ses_0"]);
    task.abort();
}

#[tokio::test]
async fn upstream_error_bodies_are_never_public() {
    let (config, _state, task) = spawn_mock(false, true).await;
    let service = service(config);
    let run = service.call(request()).await.unwrap();
    let outcome = run.completion().wait().await.unwrap();
    assert!(
        matches!(&*outcome, RunEvent::Error { code, message, .. } if code == "upstream_error" && !message.contains("secret"))
    );
    task.abort();
}

#[tokio::test]
async fn slow_housekeeping_does_not_hide_a_successful_generation() {
    let (mut config, state, task) = spawn_mock(false, false).await;
    config.request_timeout_seconds = 5;
    state.slow_delete.store(true, Ordering::Relaxed);
    let service = AgentService::new(
        OpenCode::new(config).unwrap(),
        vec![Skill {
            id:      "rewrite".into(),
            version: "1".into(),
        }],
        RuntimeConfig {
            run_timeout_seconds: 1,
            ..RuntimeConfig::default()
        },
    )
    .unwrap();
    let run = service.call(request()).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(3), run.completion().wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&*outcome, RunEvent::Completed { .. }));
    wait_for(&state, || !state.deleted.lock().unwrap().is_empty()).await;
    task.abort();
}

#[test]
fn untrusted_urls_and_invalid_limits_are_rejected() {
    for url in [
        "file:///etc/passwd",
        "http://user:secret@localhost:4096",
        "http://localhost:4096/?password=secret",
    ] {
        assert!(
            OpenCodeClient::new(OpenCodeConfig {
                base_url: url.into(),
                ..OpenCodeConfig::default()
            })
            .is_err()
        );
    }
    assert!(matches!(
        OpenCodeClient::new(OpenCodeConfig {
            poll_interval_ms: 0,
            ..OpenCodeConfig::default()
        }),
        Err(AgentError::Config(_))
    ));
}
