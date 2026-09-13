use super::*;
use nagisalake_protocol::{JobEvent, JobEventKind};

#[tokio::test]
async fn event_writes_do_not_load_or_decode_the_dispatch_payload() {
    let journal = SqliteJournal::open("sqlite::memory:").await.unwrap();
    // If the event path accidentally SELECTs/decodes dispatch_json, this row
    // fails. Event state validation must depend only on its compact head.
    sqlx::query(
        "INSERT INTO worker_jobs (job_id,dispatch_json,state,event_sequence,updated_at) VALUES \
         ('job','not JSON','received',0,0)",
    )
    .execute(&journal.pool)
    .await
    .unwrap();
    let event = JobEvent {
        job_id:    "job".into(),
        attempt:   1,
        sequence:  1,
        kind:      JobEventKind::Accepted,
        progress:  None,
        prompt_id: None,
        message:   String::new(),
        unix_ms:   1,
    };
    journal
        .call(SetPendingEvent {
            event: event.clone(),
            state: Some(JobState::Accepted),
        })
        .await
        .unwrap();
    journal
        .call(SetPendingEvent {
            event: event.clone(),
            state: None,
        })
        .await
        .unwrap();
    let mut conflicting = event.clone();
    conflicting.message = "different payload".into();
    assert!(matches!(
        journal
            .call(SetPendingEvent {
                event: conflicting,
                state: None,
            })
            .await,
        Err(JournalError::StaleEventSequence { .. })
    ));
    journal
        .call(ClearPendingEvent {
            job_id:   "job".into(),
            sequence: 0,
        })
        .await
        .unwrap();
    let stored: Option<String> =
        sqlx::query_scalar("SELECT pending_event_json FROM worker_jobs WHERE job_id='job'")
            .fetch_one(&journal.pool)
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<JobEvent>(&stored.unwrap()).unwrap(),
        event
    );
    let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(&journal.pool)
        .await
        .unwrap();
    assert_eq!(synchronous, 2, "event durability must remain FULL");
}
