use nagisalake_hub_store::{FinishAgentExecution, NewAgentExecution, PgStore, StoreConfig};
use uuid::Uuid;

#[tokio::test]
async fn agent_history_is_owner_scoped_terminal_and_expires_orphaned_runs() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping agent history PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 4,
        run_migrations: true,
    })
    .await
    .unwrap();
    let org = Uuid::new_v4().to_string();
    store
        .ensure_organization(&org, "Agent history test")
        .await
        .unwrap();
    for id in ["completed-run", "orphaned-run"] {
        store
            .create_agent_execution(NewAgentExecution {
                organization_id: &org,
                id,
                owner_id: "owner",
                actor_id: "test-actor",
                user_id: None,
                skill: "rewrite",
                skill_version: "1",
                input: "hello",
                options_json: "{}",
                now: 1,
                deadline_at: 100,
            })
            .await
            .unwrap();
    }
    assert!(
        store
            .agent_execution(&org, "someone-else", "completed-run", 2)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .agent_execution("other-org", "owner", "completed-run", 2)
            .await
            .unwrap()
            .is_none()
    );
    store
        .finish_agent_execution(FinishAgentExecution {
            organization_id: &org,
            id:              "completed-run",
            state:           "completed",
            output:          Some("result"),
            error_code:      None,
            now:             3,
        })
        .await
        .unwrap();
    // A late cancellation cannot overwrite committed output.
    store
        .finish_agent_execution(FinishAgentExecution {
            organization_id: &org,
            id:              "completed-run",
            state:           "cancelled",
            output:          None,
            error_code:      None,
            now:             4,
        })
        .await
        .unwrap();
    let record = store
        .agent_execution(&org, "owner", "completed-run", 200)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, "completed");
    assert_eq!(record.output.as_deref(), Some("result"));
    let record = store
        .agent_execution(&org, "owner", "orphaned-run", 200)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, "failed");
    assert_eq!(record.error_code.as_deref(), Some("interrupted"));
    sqlx::query("DELETE FROM organizations WHERE id=$1")
        .bind(&org)
        .execute(store.pool())
        .await
        .unwrap();
    assert!(
        store
            .agent_execution(&org, "owner", "completed-run", 200)
            .await
            .unwrap()
            .is_none()
    );
}
