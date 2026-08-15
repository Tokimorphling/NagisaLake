use super::*;

fn test_job_record(job_state: JobState, session_id: &str, last_event: u64) -> JobRecord {
    JobRecord {
        organization_id: "test-org".into(),
        actor_id: "session".into(),
        actor_kind: "browser_session".into(),
        actor_user_id: Some("creator".into()),
        worker_organization_id: "test-org".into(),
        view: JobView {
            id:                  "job-1".into(),
            workflow_id:         "workflow".into(),
            workflow_version:    "v1".into(),
            parameters:          json!({}),
            input_artifact_ids:  Vec::new(),
            output_artifact_ids: Vec::new(),
            worker_id:           "ns/node".into(),
            session_id:          session_id.into(),
            state:               job_state,
            progress:            None,
            prompt_id:           None,
            error:               None,
            events:              Vec::new(),
            created_at_unix_ms:  1,
            updated_at_unix_ms:  1,
        },
        dispatch: DispatchJob {
            command_id:       "command".into(),
            job_id:           "job-1".into(),
            attempt:          1,
            workflow_id:      "workflow".into(),
            workflow_version: "v1".into(),
            parameters:       json!({}),
            inputs:           Vec::new(),
        },
        last_event,
    }
}

/// Registers `session_id` for `ns/node` and hands back the outbound
/// receiver, which has to stay alive or the channel reads as closed.
async fn register_test_session(state: &AppState, session_id: &str) -> mpsc::Receiver<HubMessage> {
    register_org_test_session(state, "test-org", session_id, Vec::new()).await
}

async fn register_org_test_session(
    state: &AppState,
    organization_id: &str,
    session_id: &str,
    workflows: Vec<nagisalake_protocol::WorkflowCapability>,
) -> mpsc::Receiver<HubMessage> {
    let (outbound, outbound_rx) = mpsc::channel(8);
    assert!(
        state
            .sessions
            .insert(WorkerSession {
                view: WorkerView {
                    organization_id: FastStr::new(organization_id),
                    owner_user_id:   None,
                    worker_id:       "ns/node".into(),
                    session_id:      FastStr::new(session_id),
                    namespace:       "ns".into(),
                    node_name:       "node".into(),
                    capabilities:    WorkerCapabilities {
                        workflows,
                        parallelism: 1,
                        queue_depth: 1,
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
            .await
    );
    outbound_rx
}

fn accepted_event(sequence: u64) -> JobEvent {
    job_event(sequence, JobEventKind::Accepted)
}

fn job_event(sequence: u64, kind: JobEventKind) -> JobEvent {
    JobEvent {
        job_id: "job-1".into(),
        attempt: 1,
        sequence,
        kind,
        progress: None,
        prompt_id: None,
        message: String::new(),
        unix_ms: now_unix_ms(),
    }
}

async fn stored_job(state: &AppState) -> JobRecord {
    state
        .data
        .read()
        .await
        .jobs
        .get("job-1")
        .cloned()
        .expect("job stays resident while it is not terminal")
}

/// Outbox delivery updates the cached session before sending a command. The
/// cache read must be dropped first or the subsequent write self-deadlocks,
/// blocking job detail reads and every reconnect waiting on job rebind.
#[tokio::test]
async fn outbox_rebind_releases_the_cache_read_lock_before_updating() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    let _session = register_test_session(&state, "session-2").await;
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Received, "session-1", 0),
    );
    let dispatch = DispatchJob {
        command_id:       "redelivery-command".into(),
        job_id:           "job-1".into(),
        attempt:          1,
        workflow_id:      "workflow".into(),
        workflow_version: "v1".into(),
        parameters:       json!({}),
        inputs:           Vec::new(),
    };

    let rebound = tokio::time::timeout(
        Duration::from_secs(1),
        rebind_cached_outbox_job(&state, "job-1", "session-2", &dispatch),
    )
    .await
    .expect("outbox rebind must not wait on its own cache read lock")
    .expect("outbox rebind should update the in-memory job");
    assert!(rebound);

    let job = stored_job(&state).await;
    assert_eq!(job.view.session_id, "session-2");
    assert_eq!(job.dispatch.command_id, "redelivery-command");
}

#[tokio::test]
async fn outbox_rebind_merges_binding_without_rolling_back_an_accepted_event() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    let _session = register_test_session(&state, "session-1").await;
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Received, "session-1", 0),
    );
    let expected_attempt = state
        .data
        .read()
        .await
        .jobs
        .get("job-1")
        .unwrap()
        .dispatch
        .attempt;
    apply_job_event(
        &state,
        "test-org",
        "ns/node",
        "session-1",
        accepted_event(1),
    )
    .await
    .unwrap();

    let dispatch = DispatchJob {
        command_id:       "redelivery-command".into(),
        job_id:           "job-1".into(),
        attempt:          expected_attempt,
        workflow_id:      "workflow".into(),
        workflow_version: "v1".into(),
        parameters:       json!({}),
        inputs:           Vec::new(),
    };
    assert!(
        merge_cached_outbox_job(
            &state,
            "job-1",
            expected_attempt,
            "session-1",
            &dispatch,
            now_unix_ms(),
        )
        .await
    );
    let rebound = stored_job(&state).await;
    assert_eq!(rebound.view.state, JobState::Accepted);
    assert_eq!(rebound.last_event, 1);

    apply_job_event(
        &state,
        "test-org",
        "ns/node",
        "session-1",
        job_event(2, JobEventKind::Running),
    )
    .await
    .expect("the running event must still be applied after the rebind");
    let running = stored_job(&state).await;
    assert_eq!(running.view.state, JobState::Running);
    assert_eq!(running.last_event, 2);
    assert!(running.view.error.is_none());
}

#[tokio::test]
async fn outbox_cache_miss_materializes_a_bound_postgres_job() {
    let Ok(database_url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping outbox cache-miss PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is unset"
        );
        return;
    };
    let mut hub_config = config();
    hub_config.database = Some(StoreConfig {
        url:             database_url,
        max_connections: 5,
        run_migrations:  true,
    });
    let (_router, state) = router_with_state(hub_config).await.unwrap();
    let store = state.store.clone().unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let account = store
        .register_user(
            &format!("outbox-cache-{suffix}@example.com"),
            "argon2-test-hash",
            "Outbox cache organization",
        )
        .await
        .unwrap();
    let job_id = format!("outbox-cache-job-{suffix}");
    let now = now_unix_ms();
    store
        .create_job(JobUpsert {
            organization_id: &account.organization_id,
            id: &job_id,
            actor_id: &account.user.id,
            actor_kind: "browser_session",
            actor_user_id: Some(&account.user.id),
            workflow_id: "workflow",
            workflow_version: "v1",
            parameters_json: "{}",
            input_artifact_ids_json: "[]",
            output_artifact_ids_json: "[]",
            worker_id: "ns/node",
            worker_organization_id: &account.organization_id,
            session_id: "session-1",
            attempt: 1,
            state: "received",
            progress: None,
            prompt_id: None,
            error: None,
            last_event: 0,
            now,
        })
        .await
        .unwrap();
    assert!(
        !state.data.read().await.jobs.contains_key(&job_id),
        "the persisted row deliberately starts absent from the cache"
    );
    let stored = store
        .job(&account.organization_id, &job_id)
        .await
        .unwrap()
        .unwrap();
    let dispatch = DispatchJob {
        command_id:       format!("outbox-command-{suffix}"),
        job_id:           job_id.clone(),
        attempt:          1,
        workflow_id:      "workflow".into(),
        workflow_version: "v1".into(),
        parameters:       json!({}),
        inputs:           Vec::new(),
    };
    assert!(
        cache_bound_outbox_job(&state, &store, &stored, &dispatch)
            .await
            .unwrap()
    );
    let cached = state
        .data
        .read()
        .await
        .jobs
        .get(&job_id)
        .cloned()
        .expect("outbox materialization must install the bound job");
    assert_eq!(cached.view.state, JobState::Received);
    assert_eq!(cached.view.worker_id, "ns/node");
    assert_eq!(cached.view.session_id, "session-1");
    assert_eq!(cached.dispatch.command_id, dispatch.command_id);
}

#[tokio::test]
async fn restarted_queued_child_dispatches_and_accepts_its_first_worker_event() {
    let Ok(database_url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping queued restart hand-off PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is \
             unset"
        );
        return;
    };
    let mut hub_config = config();
    hub_config.database = Some(StoreConfig {
        url:             database_url,
        max_connections: 5,
        run_migrations:  true,
    });
    let (_router, state) = router_with_state(hub_config).await.unwrap();
    let store = state.store.clone().unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let account = store
        .register_user(
            &format!("queued-restart-{suffix}@example.com"),
            "argon2-test-hash",
            "Queued restart organization",
        )
        .await
        .unwrap();
    let job_id = format!("queued-restart-job-{suffix}");
    let batch_id = format!("queued-restart-batch-{suffix}");
    let now = now_unix_ms();
    assert_eq!(
        store
            .commit_new_batch(
                nagisalake_hub_store::BatchInsert {
                    batch_id:                &batch_id,
                    organization_id:         &account.organization_id,
                    actor_id:                &account.user.id,
                    actor_kind:              "browser_session",
                    actor_user_id:           Some(&account.user.id),
                    workflow_id:             "workflow",
                    workflow_version:        "v1",
                    workflow_content_digest: None,
                    base_parameters_json:    "{}",
                    variation_spec_json:     "{}",
                    device_organization_id:  Some(&account.organization_id),
                    device_id:               Some("ns/node"),
                    total_jobs:              1,
                    retry_of_batch_id:       None,
                },
                &[nagisalake_hub_store::BatchChildJob {
                    job_id:             &job_id,
                    batch_index:        0,
                    client_item_id:     None,
                    parameters_json:    "{}",
                    input_artifact_ids: &[],
                }],
                &[],
                None,
                None,
                now,
            )
            .await
            .unwrap(),
        nagisalake_hub_store::CommitBatchResult::Created
    );

    let restarted = hydrate_hub_data(&store)
        .await
        .expect("restart must hydrate an unbound queued batch child");
    let queued = restarted.jobs.get(&job_id).unwrap();
    assert_eq!(queued.view.state, JobState::Queued);
    assert!(queued.worker_organization_id.is_empty());
    assert!(queued.view.worker_id.is_empty());
    assert!(queued.view.session_id.is_empty());
    *state.data.write().await = restarted;

    let mut outbound =
        register_org_test_session(&state, &account.organization_id, "session-1", vec![
            nagisalake_protocol::WorkflowCapability {
                id:           "workflow".into(),
                version:      "v1".into(),
                output_types: Vec::new(),
                manifest:     None,
            },
        ])
        .await;
    schedule_pass(&state, &store)
        .await
        .expect("the scheduler must bind the hydrated queued child");
    let entry = store
        .claim_dispatches(now_unix_ms() + 1, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.organization_id == account.organization_id && entry.job_id == job_id)
        .expect("scheduler binding must create a dispatch outbox entry");

    let dispatch_state = state.clone();
    let dispatch_store = store.clone();
    let delivery = tokio::spawn(async move {
        dispatch_outbox_entry(&dispatch_state, &dispatch_store, entry).await
    });
    let HubMessage::DispatchJob(dispatch) =
        tokio::time::timeout(Duration::from_secs(1), outbound.recv())
            .await
            .expect("outbox delivery must not stall")
            .expect("worker outbound channel must stay open")
    else {
        panic!("expected DispatchJob from the durable outbox");
    };
    assert_eq!(dispatch.job_id, job_id);
    let pending = state
        .sessions
        .inner
        .read()
        .await
        .get(&session_key(&account.organization_id, "ns/node"))
        .unwrap()
        .pending
        .clone();
    pending
        .lock()
        .await
        .remove(&dispatch.command_id)
        .expect("outbox command must await its ACK")
        .send(CommandAck {
            command_id: dispatch.command_id.clone(),
            accepted:   true,
            message:    String::new(),
        })
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), delivery)
            .await
            .expect("accepted dispatch must finish")
            .expect("dispatch task must not panic")
    );

    let ack = apply_job_event(
        &state,
        &account.organization_id,
        "ns/node",
        "session-1",
        JobEvent {
            job_id:    job_id.clone(),
            attempt:   1,
            sequence:  1,
            kind:      JobEventKind::Accepted,
            progress:  None,
            prompt_id: None,
            message:   String::new(),
            unix_ms:   now_unix_ms(),
        },
    )
    .await
    .expect("the worker event must match the binding installed by outbox delivery");
    assert_eq!(ack.job_id, job_id);
    assert_eq!(ack.sequence, 1);
    let cached = state.data.read().await.jobs.get(&job_id).cloned().unwrap();
    assert_eq!(cached.view.state, JobState::Accepted);
    assert_eq!(cached.worker_organization_id, account.organization_id);
    assert_eq!(cached.view.worker_id, "ns/node");
    assert_eq!(cached.view.session_id, "session-1");
    assert_eq!(cached.last_event, 1);
}

#[tokio::test]
async fn cancelling_a_restart_hydrated_queued_child_removes_its_backlog_and_quota() {
    let Ok(database_url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping hydrated queued cancellation PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL \
             is unset"
        );
        return;
    };
    let mut hub_config = config();
    hub_config.database = Some(StoreConfig {
        url:             database_url,
        max_connections: 5,
        run_migrations:  true,
    });
    let (_router, state) = router_with_state(hub_config).await.unwrap();
    let store = state.store.clone().unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let account = store
        .register_user(
            &format!("queued-cancel-hub-{suffix}@example.com"),
            "argon2-test-hash",
            "Queued cancellation Hub organization",
        )
        .await
        .unwrap();
    let batch_id = format!("queued-cancel-hub-batch-{suffix}");
    let job_id = format!("queued-cancel-hub-job-{suffix}");
    store
        .commit_new_batch(
            nagisalake_hub_store::BatchInsert {
                batch_id:                &batch_id,
                organization_id:         &account.organization_id,
                actor_id:                &account.user.id,
                actor_kind:              "browser_session",
                actor_user_id:           Some(&account.user.id),
                workflow_id:             "workflow",
                workflow_version:        "v1",
                workflow_content_digest: None,
                base_parameters_json:    "{}",
                variation_spec_json:     "{}",
                device_organization_id:  None,
                device_id:               None,
                total_jobs:              1,
                retry_of_batch_id:       None,
            },
            &[nagisalake_hub_store::BatchChildJob {
                job_id:             &job_id,
                batch_index:        0,
                client_item_id:     None,
                parameters_json:    "{}",
                input_artifact_ids: &[],
            }],
            &[],
            None,
            None,
            now_unix_ms(),
        )
        .await
        .unwrap();
    *state.data.write().await = hydrate_hub_data(&store).await.unwrap();
    assert_eq!(
        state
            .data
            .read()
            .await
            .jobs
            .get(&job_id)
            .unwrap()
            .view
            .state,
        JobState::Queued
    );

    let principal = Principal {
        kind:            PrincipalKind::BrowserSession,
        actor_id:        "test-session".into(),
        user_id:         Some(account.user.id.clone()),
        organization_id: account.organization_id.clone(),
        role:            Role::Owner,
        scopes:          Default::default(),
    };
    let cancelled = cancel_job_for_principal(&state, &principal, &job_id)
        .await
        .expect("a hydrated queued child must use the atomic queued cancellation path");
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert!(!state.data.read().await.jobs.contains_key(&job_id));
    assert_eq!(
        store
            .dispatch_queue_depth(&account.organization_id)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .quota(&account.organization_id)
            .await
            .unwrap()
            .active_jobs,
        0
    );
}

#[tokio::test]
async fn a_new_forward_event_recovers_state_lost_before_its_sequence() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    let _session = register_test_session(&state, "session-1").await;
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Received, "session-1", 0),
    );

    let ack = apply_job_event(
        &state,
        "test-org",
        "ns/node",
        "session-1",
        job_event(2, JobEventKind::Running),
    )
    .await
    .expect("a forward event must recover a stale state instead of being acked and dropped");

    assert_eq!(ack.sequence, 2);
    let job = stored_job(&state).await;
    assert_eq!(job.view.state, JobState::Running);
    assert_eq!(job.last_event, 2);
    assert!(job.view.error.is_none());
}

#[tokio::test]
async fn a_new_invalid_event_is_rejected_without_an_ack() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    let _session = register_test_session(&state, "session-1").await;
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Running, "session-1", 2),
    );

    assert!(matches!(
        apply_job_event(
            &state,
            "test-org",
            "ns/node",
            "session-1",
            job_event(3, JobEventKind::Accepted),
        )
        .await,
        Err(HubError::Conflict(_))
    ));

    let job = stored_job(&state).await;
    assert_eq!(job.view.state, JobState::Running);
    assert_eq!(job.last_event, 2);
    assert!(job.view.error.is_none());
}

/// The stall this reproduces: a job parked at `received` with `last_event`
/// already at 1 because the accepted event reached the event log but not the
/// job row before event and job persistence became atomic. An event dropped as
/// "stale session" was then acked without being applied at all. The worker kept
/// replaying the event, the Hub kept dismissing every replay as a duplicate,
/// and the job never moved again.
#[tokio::test]
async fn a_replayed_event_recovers_a_job_whose_state_lags_its_event_stream() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    let _session = register_test_session(&state, "session-1").await;
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Received, "session-1", 1),
    );

    let ack = apply_job_event(
        &state,
        "test-org",
        "ns/node",
        "session-1",
        accepted_event(1),
    )
    .await
    .unwrap();

    assert_eq!(ack.sequence, 1);
    let job = stored_job(&state).await;
    assert_eq!(
        job.view.state,
        JobState::Accepted,
        "a replay must still carry the state the event implies"
    );
    assert_eq!(job.last_event, 1, "a replay must not advance the sequence");
    assert!(
        job.view.events.is_empty(),
        "the event is already on record; it must not be appended twice"
    );
}

/// After a reconnect the job row still names the previous session. Rejecting
/// the new session's events looked safe but was terminal: the ack sent along
/// with the rejection makes the worker clear its pending event, so the
/// update is gone for good and the job freezes.
#[tokio::test]
async fn events_from_a_reconnected_session_are_adopted_rather_than_dropped() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    let _session = register_test_session(&state, "session-2").await;
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Received, "session-1", 0),
    );

    apply_job_event(
        &state,
        "test-org",
        "ns/node",
        "session-2",
        accepted_event(1),
    )
    .await
    .unwrap();

    let job = stored_job(&state).await;
    assert_eq!(job.view.state, JobState::Accepted);
    assert_eq!(job.last_event, 1);
    assert_eq!(
        job.view.session_id, "session-2",
        "the job must follow the worker to its live session"
    );
}

/// The flip side: the registry can already point at the replacement while the
/// cached job still names the old socket. The old socket must neither write to
/// the job nor receive an ack that clears the pending event.
#[tokio::test]
async fn events_from_a_superseded_session_are_rejected_without_an_ack() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    let _session = register_test_session(&state, "session-2").await;
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Received, "session-1", 0),
    );

    assert!(matches!(
        apply_job_event(
            &state,
            "test-org",
            "ns/node",
            "session-1",
            accepted_event(1),
        )
        .await,
        Err(HubError::Conflict(_))
    ));

    let job = stored_job(&state).await;
    assert_eq!(job.view.state, JobState::Received);
    assert_eq!(job.last_event, 0);
    assert_eq!(job.view.session_id, "session-1");
}

/// A replay that no longer fits the recorded state is just old news arriving
/// late. It must not rewrite the state backwards or poison `error`.
#[tokio::test]
async fn a_replayed_event_never_moves_a_job_backwards() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    let _session = register_test_session(&state, "session-1").await;
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Running, "session-1", 4),
    );

    apply_job_event(
        &state,
        "test-org",
        "ns/node",
        "session-1",
        accepted_event(1),
    )
    .await
    .unwrap();

    let job = stored_job(&state).await;
    assert_eq!(job.view.state, JobState::Running);
    assert_eq!(job.last_event, 4);
    assert!(job.view.error.is_none());
}

/// Cancelling addressed `record.view.session_id`, which after a reconnect
/// names a socket nobody reads. With no session left the send waited out the
/// full ACK timeout and then failed, leaving the job pinned and still
/// charged against the organization's quota.
#[tokio::test]
async fn cancelling_a_job_whose_worker_is_gone_does_not_wait_for_an_ack() {
    let (_router, state) = router_with_state(config()).await.unwrap();
    state.data.write().await.jobs.insert(
        "job-1".into(),
        test_job_record(JobState::Accepted, "session-1", 1),
    );
    let principal = Principal {
        kind:            PrincipalKind::BrowserSession,
        actor_id:        "session".into(),
        user_id:         Some("creator".into()),
        organization_id: "test-org".into(),
        role:            Role::Owner,
        scopes:          Default::default(),
    };

    let view = tokio::time::timeout(
        Duration::from_secs(1),
        cancel_job_for_principal(&state, &principal, "job-1"),
    )
    .await
    .expect("cancellation must not wait on a session that is not there")
    .expect("a disconnected worker must not make a job uncancellable");

    assert_eq!(view.state, JobState::Cancelled);
    assert_eq!(
        stored_job(&state).await.view.state,
        JobState::Cancelled,
        "the local cancellation must be visible to later reads"
    );
}
