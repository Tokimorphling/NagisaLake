use super::{OpenCodeClient, OpenCodeConfig, reconcile::Reconciler, router::EventRouter};
use crate::{AgentError, Backend, Completion, Execution, Progress};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{OnceCell, broadcast},
    time::MissedTickBehavior,
};

struct Inner {
    client: OpenCodeClient,
    router: OnceCell<EventRouter>,
}

#[derive(Clone)]
pub struct OpenCode {
    inner: Arc<Inner>,
}

impl OpenCode {
    /// Construction validates config/secrets without contacting the server.
    /// An optional, unavailable OpenCode must not prevent the Hub from starting.
    pub fn new(config: OpenCodeConfig) -> Result<Self, AgentError> {
        Ok(Self {
            inner: Arc::new(Inner {
                client: OpenCodeClient::new(config)?,
                router: OnceCell::new(),
            }),
        })
    }

    pub fn client(&self) -> &OpenCodeClient {
        &self.inner.client
    }
}

impl Backend for OpenCode {
    async fn execute(&self, execution: Execution) -> Result<Completion, AgentError> {
        let client = self.client();
        let router = self
            .inner
            .router
            .get_or_init(|| EventRouter::start(client.clone()))
            .await;
        let session_id = client.create_session(&execution).await?;
        let mut lease = SessionLease {
            client: client.clone(),
            session_id,
            abort: true,
        };
        // Register routing before prompt_async; events buffer while that HTTP
        // call is in flight. This also isolates concurrent tenants' sessions.
        let mut upstream = router.subscribe(&lease.session_id);
        tracing::debug!(execution_id = %execution.id, opencode_session_id = %lease.session_id,
            skill = %execution.skill.id, "created isolated agent session");
        client.prompt(&lease.session_id, &execution).await?;
        let mut reconcile = Reconciler::new(execution.skill.id.clone(), execution.max_output_bytes);
        let mut poll =
            tokio::time::interval(Duration::from_millis(client.config().poll_interval_ms));
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut upstream_open = true;
        loop {
            tokio::select! {
                _ = poll.tick() => {
                    match client.messages(&lease.session_id).await {
                        Ok(messages) => {
                            if let Some(completion) = reconcile.messages(&messages, &execution.events).await? {
                                // Cleanup is best effort and must not turn a
                                // completed generation into a timeout. Drop
                                // schedules it; short-lived clients can drain it.
                                lease.abort = false;
                                return Ok(completion);
                            }
                        }
                        // Transient poll failure is recoverable. Overall execution
                        // deadline remains enforced by AgentService around us.
                        Err(AgentError::Transport(_) | AgentError::UpstreamStatus { status: 429 | 500..=599, .. }) => {
                            execution.events.send(Progress::Warning { code: "poll_retry".into() }).await?;
                        }
                        Err(error) => return Err(error),
                    }
                }
                event = upstream.recv(), if upstream_open => {
                    match event {
                        Ok(event) => {
                            reconcile.event(&event, &execution.events).await?;
                            if matches!(event.kind.as_str(), "session.idle" | "session.status") {
                                poll.reset_immediately();
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            execution.events.send(Progress::Warning { code: "upstream_lagged".into() }).await?;
                            poll.reset_immediately();
                        }
                        Err(broadcast::error::RecvError::Closed) => { upstream_open = false; }
                    }
                }
            }
        }
    }
}

/// Async Drop is unavailable. This guard is installed immediately after the
/// session id arrives, so dropping *any* startup/poll/send future schedules a
/// bounded abort, including cancellation during prompt_async or backpressure.
struct SessionLease {
    client:     OpenCodeClient,
    session_id: String,
    abort:      bool,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.client
            .schedule_cleanup(self.session_id.clone(), self.abort);
    }
}
