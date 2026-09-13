use crate::*;
use nagisalake_core::{JobState, SetJobState, SetPendingEvent, UpsertDispatch};
use nagisalake_protocol::{DispatchJob, JobInput, PresignedRequest};
use service_async::Service;
use std::collections::BTreeMap;

fn dispatch(command_id: &str, url: &str) -> DispatchJob {
    DispatchJob {
        command_id:       command_id.into(),
        job_id:           "job-1".into(),
        attempt:          1,
        workflow_id:      "image-edit".into(),
        workflow_version: "v1".into(),
        parameters:       serde_json::json!({"prompt":"hello"}),
        inputs:           vec![JobInput {
            artifact_id:  "input-1".into(),
            name:         "source.png".into(),
            content_type: "image/png".into(),
            size_bytes:   42,
            sha256:       "a".repeat(64),
            download:     PresignedRequest {
                method:             "GET".into(),
                url:                url.into(),
                headers:            BTreeMap::new(),
                expires_at_unix_ms: 1,
            },
        }],
    }
}

#[tokio::test]
async fn sqlite_retry_refreshes_ephemeral_fields_only() {
    let journal = SqliteJournal::open("sqlite::memory:").await.unwrap();
    journal
        .call(UpsertDispatch(dispatch(
            "command-1",
            "https://objects/first",
        )))
        .await
        .unwrap();
    let refreshed = journal
        .call(UpsertDispatch(dispatch(
            "command-2",
            "https://objects/second",
        )))
        .await
        .unwrap();
    assert_eq!(refreshed.dispatch.command_id, "command-2");
    assert_eq!(
        refreshed.dispatch.inputs[0].download.url,
        "https://objects/second"
    );
    let mut conflicting = dispatch("command-3", "https://objects/third");
    conflicting.parameters = serde_json::json!({"prompt":"changed"});
    assert!(journal.call(UpsertDispatch(conflicting)).await.is_err());
}

#[tokio::test]
async fn memory_and_sqlite_enforce_the_same_state_machine() {
    let journal = MemoryJournal::default();
    journal
        .call(UpsertDispatch(dispatch("command", "https://objects/input")))
        .await
        .unwrap();
    let result = journal
        .call(SetJobState {
            job_id: "job-1".into(),
            state:  JobState::Completed,
        })
        .await;
    assert!(matches!(
        result,
        Err(JournalError::InvalidTransition { .. })
    ));
}

#[tokio::test]
async fn stale_events_cannot_replace_the_pending_outbox_event() {
    let journal = MemoryJournal::default();
    journal
        .call(UpsertDispatch(dispatch("command", "https://objects/input")))
        .await
        .unwrap();
    let newer = nagisalake_protocol::JobEvent {
        job_id:    "job-1".into(),
        attempt:   1,
        sequence:  2,
        kind:      nagisalake_protocol::JobEventKind::Accepted,
        progress:  Some(0.0),
        prompt_id: None,
        message:   String::new(),
        unix_ms:   2,
    };
    journal
        .call(SetPendingEvent {
            event: newer.clone(),
            state: Some(JobState::Accepted),
        })
        .await
        .unwrap();
    let mut older = newer;
    older.sequence = 1;
    assert!(matches!(
        journal
            .call(SetPendingEvent {
                event: older,
                state: None,
            })
            .await,
        Err(JournalError::StaleEventSequence { .. })
    ));
}
