use super::*;
use service_async::Service;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
struct Echo(Arc<AtomicUsize>);
impl Backend for Echo {
    async fn execute(&self, execution: Execution) -> Result<Completion, AgentError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        execution
            .events
            .send(Progress::TextDelta {
                text: execution.input.clone(),
            })
            .await?;
        Ok(Completion {
            text: execution.input,
        })
    }
}

fn request(skill: &str) -> RunRequest {
    RunRequest {
        skill:   skill.into(),
        input:   "hello".into(),
        options: BTreeMap::new(),
    }
}
fn skills() -> Vec<Skill> {
    vec![Skill {
        id:      "rewrite".into(),
        version: "1".into(),
    }]
}

#[tokio::test]
async fn generic_service_enforces_allowlist_before_invoking_backend() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service =
        AgentService::new(Echo(calls.clone()), skills(), RuntimeConfig::default()).unwrap();
    assert!(matches!(
        service.call(request("shell")).await,
        Err(AgentError::UnsupportedSkill)
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let mut run = service.call(request("rewrite")).await.unwrap();
    assert!(matches!(run.recv().await, Some(RunEvent::Started { .. })));
    assert_eq!(
        run.recv().await,
        Some(RunEvent::TextDelta {
            text: "hello".into(),
        })
    );
    assert!(matches!(run.recv().await, Some(RunEvent::Completed { text, .. }) if text == "hello"));
    assert!(run.recv().await.is_none());
}

#[derive(Clone)]
struct Flood;
impl Backend for Flood {
    async fn execute(&self, execution: Execution) -> Result<Completion, AgentError> {
        loop {
            execution
                .events
                .send(Progress::TextDelta { text: "x".into() })
                .await?;
        }
    }
}

#[tokio::test]
async fn full_progress_queue_does_not_block_cancel_or_terminal_observer() {
    let config = RuntimeConfig {
        max_concurrent_runs: 1,
        event_buffer: 1,
        ..RuntimeConfig::default()
    };
    let service = AgentService::new(Flood, skills(), config).unwrap();
    let mut run = service.call(request("rewrite")).await.unwrap();
    assert!(matches!(run.recv().await, Some(RunEvent::Started { .. })));
    assert!(matches!(
        service.call(request("rewrite")).await,
        Err(AgentError::Busy)
    ));
    let mut completion = run.completion();
    run.cancel();
    let event = tokio::time::timeout(Duration::from_secs(1), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&*event, RunEvent::Cancelled { .. }));
    // Slot is released by execution completion, not by retaining the handle.
    tokio::task::yield_now().await;
    assert!(service.call(request("rewrite")).await.is_ok());
}

#[tokio::test]
async fn dropping_consumer_cancels_even_if_an_observer_is_retained() {
    let service = AgentService::new(Flood, skills(), RuntimeConfig::default()).unwrap();
    let run = service.call(request("rewrite")).await.unwrap();
    let mut completion = run.completion();
    drop(run);
    assert!(matches!(
        &*completion.wait().await.unwrap(),
        RunEvent::Cancelled { .. }
    ));
}

#[tokio::test]
async fn timeout_is_independent_of_downstream_backpressure() {
    let config = RuntimeConfig {
        run_timeout_seconds: 1,
        event_buffer: 1,
        ..RuntimeConfig::default()
    };
    let service = AgentService::new(Flood, skills(), config).unwrap();
    let run = service.call(request("rewrite")).await.unwrap();
    let mut completion = run.completion();
    let event = tokio::time::timeout(Duration::from_secs(3), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&*event, RunEvent::Error { code, .. } if code == "timeout"));
}

#[test]
fn public_request_rejects_provider_and_tool_overrides() {
    let request = r#"{"skill":"rewrite","input":"hello","agent":"build","tools":{"bash":true}}"#;
    assert!(serde_json::from_str::<RunRequest>(request).is_err());
    assert!(
        AgentService::new(
            Echo(Arc::default()),
            vec![Skill {
                id:      "*".into(),
                version: "1".into(),
            }],
            RuntimeConfig::default()
        )
        .is_err()
    );
}
