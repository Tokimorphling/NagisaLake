use super::*;

/// Admission has to be atomic. Two submissions arriving together must not
/// both observe the same free slot and both dispatch, or the worker is handed
/// more work than `parallelism + queue_depth` allows.
#[tokio::test]
async fn capacity_reservations_prevent_concurrent_overshoot() {
    let sessions = SessionRegistry::default();
    let (outbound, _outbound_rx) = mpsc::channel(4);

    // One execution slot plus room for two queued jobs: three admissions.
    let view = WorkerView {
        organization_id: "org".into(),
        owner_user_id:   None,
        worker_id:       "ns/node".into(),
        session_id:      "session-1".into(),
        namespace:       "ns".into(),
        node_name:       "node".into(),
        capabilities:    WorkerCapabilities {
            workflows: Vec::new(),
            parallelism: 1,
            queue_depth: 2,
            supports_queued_job_cancellation: true,
            labels: BTreeMap::new(),
        },
        active_jobs:     0,
        queued_jobs:     0,
        connected_at:    now_unix_ms(),
    };
    assert!(
        sessions
            .insert(WorkerSession {
                view: view.clone(),
                credential_id: None,
                outbound: outbound.clone(),
                pending: Arc::new(Mutex::new(HashMap::new())),
                pending_capacity_reservations: HashSet::new(),
                confirmed_capacity_reservations: HashSet::new(),
                disconnect: CancellationToken::new(),
                last_seen: Instant::now(),
            })
            .await
    );

    // Three reservations fit; the fourth must be refused even though the
    // worker has not reported any load yet.
    for index in 0..3 {
        assert!(
            sessions
                .reserve_capacity(&format!("command-{index}"), |_view| true)
                .await
                .is_some(),
            "reservation {index} should fit in parallelism + queue_depth"
        );
    }
    assert!(
        sessions
            .reserve_capacity("command-overflow", |_view| true)
            .await
            .is_none(),
        "capacity must be enforced from reservations alone, before any heartbeat"
    );

    // Reservations are visible as queued work so the console does not show
    // idle capacity that admission has already spent.
    let listed = sessions.list_matching(|_view| true).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].queued_jobs, 3);

    // A rejected dispatch frees its slot immediately.
    sessions
        .settle_capacity_reservation("org", "ns/node", "session-1", "command-0", false)
        .await;
    assert!(
        sessions
            .reserve_capacity("command-3", |_view| true)
            .await
            .is_some(),
        "a rejected dispatch must return its slot"
    );

    // An accepted dispatch stays charged until the worker reports it, so the
    // window between ACK and heartbeat cannot be double-booked.
    sessions
        .settle_capacity_reservation("org", "ns/node", "session-1", "command-1", true)
        .await;
    assert!(
        sessions
            .reserve_capacity("command-4", |_view| true)
            .await
            .is_none(),
        "an accepted dispatch must remain charged until the next heartbeat"
    );

    // The heartbeat is authoritative: it replaces the confirmed set with the
    // worker's own counts. Here the worker reports one running job.
    sessions
        .update_heartbeat("org", "ns/node", "session-1", 1, 0)
        .await
        .unwrap();
    let listed = sessions.list_matching(|_view| true).await;
    // Two reservations are still pending an ACK, plus the reported job.
    assert_eq!(listed[0].active_jobs, 1);
    assert_eq!(listed[0].queued_jobs, 2);
    assert!(
        sessions
            .reserve_capacity("command-5", |_view| true)
            .await
            .is_none(),
        "reported load plus outstanding reservations still fill the worker"
    );

    // Releasing the two undelivered dispatches leaves only the running job.
    for command in ["command-2", "command-3"] {
        sessions
            .release_capacity_reservation("org", "ns/node", "session-1", command)
            .await;
    }
    assert!(
        sessions
            .reserve_capacity("command-6", |_view| true)
            .await
            .is_some(),
        "one running job against capacity 3 leaves room"
    );
}

/// A dispatch that times out must not free its slot: the worker may have
/// accepted it and simply not answered in time. Releasing it would let the
/// Hub admit past the worker's real limit.
#[tokio::test]
async fn a_timed_out_dispatch_stays_charged_until_a_heartbeat_resolves_it() {
    let sessions = SessionRegistry::default();
    let (outbound, _outbound_rx) = mpsc::channel(4);
    let view = WorkerView {
        organization_id: "org".into(),
        owner_user_id:   None,
        worker_id:       "ns/node".into(),
        session_id:      "session-1".into(),
        namespace:       "ns".into(),
        node_name:       "node".into(),
        capabilities:    WorkerCapabilities {
            workflows: Vec::new(),
            parallelism: 1,
            queue_depth: 0,
            supports_queued_job_cancellation: true,
            labels: BTreeMap::new(),
        },
        active_jobs:     0,
        queued_jobs:     0,
        connected_at:    now_unix_ms(),
    };
    sessions
        .insert(WorkerSession {
            view,
            credential_id: None,
            outbound,
            pending: Arc::new(Mutex::new(HashMap::new())),
            pending_capacity_reservations: HashSet::new(),
            confirmed_capacity_reservations: HashSet::new(),
            disconnect: CancellationToken::new(),
            last_seen: Instant::now(),
        })
        .await;

    assert!(
        sessions
            .reserve_capacity("command-0", |_view| true)
            .await
            .is_some()
    );
    sessions
        .mark_capacity_reservation_uncertain("org", "ns/node", "session-1", "command-0")
        .await;
    assert!(
        sessions
            .reserve_capacity("command-1", |_view| true)
            .await
            .is_none(),
        "an unresolved dispatch must keep occupying the slot"
    );

    // The heartbeat settles it. The worker never took the job, so its report
    // shows no load and the slot becomes usable again.
    sessions
        .update_heartbeat("org", "ns/node", "session-1", 0, 0)
        .await
        .unwrap();
    assert!(
        sessions
            .reserve_capacity("command-2", |_view| true)
            .await
            .is_some(),
        "the heartbeat is authoritative and clears an uncertain reservation"
    );
}

/// Builds a session for `organization_id`/`ns/node` owned by `credential_id`.
fn credential_session(
    session_id: &'static str,
    credential_id: Option<&str>,
    outbound: mpsc::Sender<HubMessage>,
    disconnect: CancellationToken,
) -> WorkerSession {
    WorkerSession {
        view: WorkerView {
            organization_id: "org".into(),
            owner_user_id:   None,
            worker_id:       "ns/node".into(),
            session_id:      session_id.into(),
            namespace:       "ns".into(),
            node_name:       "node".into(),
            capabilities:    WorkerCapabilities {
                workflows: Vec::new(),
                parallelism: 1,
                queue_depth: 0,
                supports_queued_job_cancellation: true,
                labels: BTreeMap::new(),
            },
            active_jobs:     0,
            queued_jobs:     0,
            connected_at:    now_unix_ms(),
        },
        credential_id: credential_id.map(str::to_owned),
        outbound,
        pending: Arc::new(Mutex::new(HashMap::new())),
        pending_capacity_reservations: HashSet::new(),
        confirmed_capacity_reservations: HashSet::new(),
        disconnect,
        last_seen: Instant::now(),
    }
}

/// Revoking a credential has to do both halves of the job: drop the sessions
/// holding it now, and keep the reconnect that follows from getting back in.
/// `insert` holds the `revoked_credentials` read guard across its write into
/// `inner` to close the gap between the two — release it early and a session
/// can land just after the sweep walked past it, staying connected on a
/// credential the operator already deleted.
#[tokio::test]
async fn revoking_a_credential_drops_its_session_and_blocks_the_reconnect() {
    let sessions = SessionRegistry::default();
    let (outbound, mut outbound_rx) = mpsc::channel(4);
    let disconnect = CancellationToken::new();
    assert!(
        sessions
            .insert(credential_session(
                "session-1",
                Some("cred-1"),
                outbound,
                disconnect.clone(),
            ))
            .await
    );
    assert_eq!(sessions.count().await, 1);

    sessions.disconnect_credential("cred-1").await;

    assert_eq!(
        sessions.count().await,
        0,
        "revocation must drop the live session"
    );
    assert!(
        disconnect.is_cancelled(),
        "the connection actor must be told to tear the socket down"
    );
    match outbound_rx.try_recv() {
        Ok(HubMessage::Error(error)) => {
            assert_eq!(error.code, "credential_revoked");
            assert!(!error.retryable, "a revoked credential must not retry");
        }
        other => panic!("expected a credential_revoked error, got {other:?}"),
    }

    let (outbound, _outbound_rx) = mpsc::channel(4);
    assert!(
        !sessions
            .insert(credential_session(
                "session-2",
                Some("cred-1"),
                outbound,
                CancellationToken::new(),
            ))
            .await,
        "a revoked credential must not be able to register again"
    );
    assert_eq!(sessions.count().await, 0);
}

/// Reservations belong to one session. A reconnect issues a new session id,
/// and stale settle calls from the previous one must not touch it.
#[tokio::test]
async fn capacity_reservations_are_scoped_to_a_session() {
    let sessions = SessionRegistry::default();
    let (outbound, _outbound_rx) = mpsc::channel(4);
    let view = WorkerView {
        organization_id: "org".into(),
        owner_user_id:   None,
        worker_id:       "ns/node".into(),
        session_id:      "session-2".into(),
        namespace:       "ns".into(),
        node_name:       "node".into(),
        capabilities:    WorkerCapabilities {
            workflows: Vec::new(),
            parallelism: 1,
            queue_depth: 0,
            supports_queued_job_cancellation: true,
            labels: BTreeMap::new(),
        },
        active_jobs:     0,
        queued_jobs:     0,
        connected_at:    now_unix_ms(),
    };
    sessions
        .insert(WorkerSession {
            view,
            credential_id: None,
            outbound,
            pending: Arc::new(Mutex::new(HashMap::new())),
            pending_capacity_reservations: HashSet::new(),
            confirmed_capacity_reservations: HashSet::new(),
            disconnect: CancellationToken::new(),
            last_seen: Instant::now(),
        })
        .await;

    assert!(
        sessions
            .reserve_capacity("command-0", |_view| true)
            .await
            .is_some()
    );
    // A settle for the previous session must be ignored.
    sessions
        .settle_capacity_reservation("org", "ns/node", "session-1", "command-0", false)
        .await;
    assert!(
        sessions
            .reserve_capacity("command-1", |_view| true)
            .await
            .is_none(),
        "a stale session must not release the current session's reservation"
    );
    // Heartbeats from the stale session are rejected outright.
    assert!(
        sessions
            .update_heartbeat("org", "ns/node", "session-1", 0, 0)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ensuring_an_exact_durable_reservation_is_idempotent_and_capacity_bounded() {
    let sessions = SessionRegistry::default();
    let (outbound, _outbound_rx) = mpsc::channel(4);
    sessions
        .insert(WorkerSession {
            view: WorkerView {
                organization_id: "org".into(),
                owner_user_id:   None,
                worker_id:       "ns/node".into(),
                session_id:      "session-1".into(),
                namespace:       "ns".into(),
                node_name:       "node".into(),
                capabilities:    WorkerCapabilities {
                    workflows: Vec::new(),
                    parallelism: 1,
                    queue_depth: 0,
                    supports_queued_job_cancellation: true,
                    labels: BTreeMap::new(),
                },
                active_jobs:     0,
                queued_jobs:     0,
                connected_at:    now_unix_ms(),
            },
            credential_id: None,
            outbound,
            pending: Arc::new(Mutex::new(HashMap::new())),
            pending_capacity_reservations: HashSet::new(),
            confirmed_capacity_reservations: HashSet::new(),
            disconnect: CancellationToken::new(),
            last_seen: Instant::now(),
        })
        .await;

    assert!(
        sessions
            .ensure_capacity_reservation("org", "ns/node", "session-1", "dispatch:job-1:1")
            .await
    );
    assert!(
        sessions
            .ensure_capacity_reservation("org", "ns/node", "session-1", "dispatch:job-1:1")
            .await,
        "an outbox retry must reuse its existing reservation"
    );
    assert!(
        !sessions
            .ensure_capacity_reservation("org", "ns/node", "session-1", "dispatch:job-2:1")
            .await,
        "a different durable command cannot overbook parallelism=1"
    );
    sessions
        .settle_capacity_reservation("org", "ns/node", "session-1", "dispatch:job-1:1", false)
        .await;
    assert!(
        sessions
            .ensure_capacity_reservation("org", "ns/node", "session-1", "dispatch:job-2:1")
            .await,
        "settling the first command must free the slot for the next job"
    );
}

/// The predicate decides which workers are eligible, and an ineligible
/// worker must never be charged.
#[tokio::test]
async fn reservations_only_touch_workers_the_predicate_accepts() {
    let sessions = SessionRegistry::default();
    let (outbound, _outbound_rx) = mpsc::channel(4);
    for worker_id in ["ns/a", "ns/b"] {
        let view = WorkerView {
            organization_id: "org".into(),
            owner_user_id:   None,
            worker_id:       worker_id.into(),
            session_id:      format!("session-{worker_id}").into(),
            namespace:       "ns".into(),
            node_name:       worker_id.into(),
            capabilities:    WorkerCapabilities {
                workflows: Vec::new(),
                parallelism: 1,
                queue_depth: 0,
                supports_queued_job_cancellation: true,
                labels: BTreeMap::new(),
            },
            active_jobs:     0,
            queued_jobs:     0,
            connected_at:    now_unix_ms(),
        };
        sessions
            .insert(WorkerSession {
                view,
                credential_id: None,
                outbound: outbound.clone(),
                pending: Arc::new(Mutex::new(HashMap::new())),
                pending_capacity_reservations: HashSet::new(),
                confirmed_capacity_reservations: HashSet::new(),
                disconnect: CancellationToken::new(),
                last_seen: Instant::now(),
            })
            .await;
    }

    let selected = sessions
        .reserve_capacity("command-0", |view| view.worker_id == "ns/b")
        .await
        .expect("the eligible worker should be chosen");
    assert_eq!(selected.worker_id, "ns/b");

    // The rejected worker is untouched, so it can still be reserved.
    let selected = sessions
        .reserve_capacity("command-1", |view| view.worker_id == "ns/a")
        .await
        .expect("the other worker must not have been charged");
    assert_eq!(selected.worker_id, "ns/a");

    // No worker matches, so nothing is charged.
    assert!(
        sessions
            .reserve_capacity("command-2", |view| view.worker_id == "ns/missing")
            .await
            .is_none()
    );
}

/// A worker whose socket is half-open never closes it, so silence is the
/// only available liveness signal. Sessions that stop heart-beating must be
/// dropped, or they keep reading as connected and keep receiving jobs.
#[tokio::test]
async fn stale_worker_sessions_are_reaped() {
    let sessions = SessionRegistry::default();
    let (outbound, _outbound_rx) = mpsc::channel(4);
    let disconnect = CancellationToken::new();

    let mut view = WorkerView {
        organization_id: "org".into(),
        owner_user_id:   None,
        worker_id:       "ns/node".into(),
        session_id:      "session-1".into(),
        namespace:       "ns".into(),
        node_name:       "node".into(),
        capabilities:    WorkerCapabilities {
            workflows: Vec::new(),
            parallelism: 1,
            queue_depth: 0,
            supports_queued_job_cancellation: false,
            labels: BTreeMap::new(),
        },
        active_jobs:     0,
        queued_jobs:     0,
        connected_at:    now_unix_ms(),
    };
    view.worker_id = "ns/node".into();

    assert!(
        sessions
            .insert(WorkerSession {
                view: view.clone(),
                credential_id: None,
                outbound: outbound.clone(),
                pending: Arc::new(Mutex::new(HashMap::new())),
                pending_capacity_reservations: HashSet::new(),
                confirmed_capacity_reservations: HashSet::new(),
                disconnect: disconnect.clone(),
                last_seen: Instant::now(),
            })
            .await
    );
    assert_eq!(sessions.count().await, 1);

    // A fresh session survives.
    assert!(
        sessions
            .reap_stale(Duration::from_secs(45))
            .await
            .is_empty()
    );
    assert_eq!(sessions.count().await, 1);

    // A heartbeat refreshes the deadline.
    sessions
        .update_heartbeat("org", "ns/node", "session-1", 1, 0)
        .await
        .unwrap();
    assert!(
        sessions
            .reap_stale(Duration::from_secs(45))
            .await
            .is_empty()
    );

    // Silence past the allowance drops it and signals the connection actor.
    let reaped = sessions.reap_stale(Duration::ZERO).await;
    assert_eq!(reaped, vec!["ns/node".to_string()]);
    assert_eq!(sessions.count().await, 0);
    assert!(
        disconnect.is_cancelled(),
        "reaping must cancel the session so the socket is torn down"
    );

    // A reaped session must no longer accept heartbeats.
    assert!(
        sessions
            .update_heartbeat("org", "ns/node", "session-1", 0, 0)
            .await
            .is_err()
    );
}
