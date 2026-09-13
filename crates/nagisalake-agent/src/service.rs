use crate::{
    AgentError, Completion, CompletionReceiver, EventSender, Execution, RunEvent, RunHandle,
    RunRequest, Skill,
};
use futures_util::FutureExt;
use serde::Deserialize;
use service_async::Service;
use std::{
    collections::BTreeMap,
    future::Future,
    panic::AssertUnwindSafe,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Statically dispatched provider contract. An execution future must be safe
/// to drop: timeout, downstream disconnect and cancellation all drop it. A
/// backend owns cleanup/abort of any upstream work it already started.
pub trait Backend: Clone + Send + Sync + 'static {
    fn execute(
        &self,
        execution: Execution,
    ) -> impl Future<Output = Result<Completion, AgentError>> + Send;
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub run_timeout_seconds: u64,
    pub max_concurrent_runs: usize,
    pub event_buffer:        usize,
    pub max_input_bytes:     usize,
    pub max_output_bytes:    usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            run_timeout_seconds: 120,
            max_concurrent_runs: 16,
            event_buffer:        256,
            max_input_bytes:     64 * 1024,
            max_output_bytes:    1024 * 1024,
        }
    }
}

impl RuntimeConfig {
    pub fn validate(&self) -> Result<(), AgentError> {
        if !(1..=3600).contains(&self.run_timeout_seconds)
            || !(1..=1024).contains(&self.max_concurrent_runs)
            || !(1..=4096).contains(&self.event_buffer)
            || !(1..=1024 * 1024).contains(&self.max_input_bytes)
            || !(1..=16 * 1024 * 1024).contains(&self.max_output_bytes)
        {
            return Err(AgentError::Config(
                "runtime limits are outside supported bounds",
            ));
        }
        Ok(())
    }
}

/// Skill allowlist + bounded execution policy composed over a concrete backend.
/// Authorization and tenant quotas remain the responsibility of the caller.
#[derive(Clone)]
pub struct AgentService<B> {
    backend: B,
    skills:  Arc<BTreeMap<String, Skill>>,
    config:  RuntimeConfig,
    permits: Arc<Semaphore>,
}

impl<B: Backend> AgentService<B> {
    pub fn new(backend: B, skills: Vec<Skill>, config: RuntimeConfig) -> Result<Self, AgentError> {
        config.validate()?;
        let mut catalog = BTreeMap::new();
        for skill in skills {
            if skill.id.is_empty()
                || skill.id.len() > 128
                || skill.version.is_empty()
                || !skill
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
            {
                return Err(AgentError::Config(
                    "skill ids must be literal names, not patterns or paths",
                ));
            }
            if catalog.insert(skill.id.clone(), skill).is_some() {
                return Err(AgentError::Config("duplicate skill id"));
            }
        }
        Ok(Self {
            backend,
            skills: Arc::new(catalog),
            permits: Arc::new(Semaphore::new(config.max_concurrent_runs)),
            config,
        })
    }

    pub fn skills(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    pub fn validate_request(&self, request: &RunRequest) -> Result<(), AgentError> {
        if !self.skills.contains_key(&request.skill) {
            return Err(AgentError::UnsupportedSkill);
        }
        if request.input.trim().is_empty() || request.input.len() > self.config.max_input_bytes {
            return Err(AgentError::InvalidRequest("input is empty or too large"));
        }
        if request.options.len() > 16
            || serde_json::to_vec(&request.options)
                .map_err(|_| AgentError::InvalidRequest("invalid options"))?
                .len()
                > 8192
        {
            return Err(AgentError::InvalidRequest("options are too large"));
        }
        Ok(())
    }
    /// Admission is synchronous and performs no upstream I/O, allowing the Hub
    /// to register tenant ownership atomically before yielding to other requests.
    pub fn start(&self, request: RunRequest) -> Result<RunHandle, AgentError> {
        self.validate_request(&request)?;
        // Reject instead of queuing an unbounded number of futures/prompts.
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| AgentError::Busy)?;
        let id = Uuid::new_v4().to_string();
        let cancellation = CancellationToken::new();
        let (sender, receiver) = mpsc::channel(self.config.event_buffer);
        let (finished, completion) = watch::channel(None);
        let handle = RunHandle {
            id: id.clone(),
            receiver,
            completion: CompletionReceiver {
                receiver: completion,
            },
            cancellation: cancellation.clone(),
            ended: false,
        };
        let backend = self.backend.clone();
        let config = self.config.clone();
        let skill = self.skills[&request.skill].clone();
        tokio::spawn(async move {
            let _permit = permit;
            let progress = EventSender {
                sender:         sender.clone(),
                text_bytes:     Arc::new(AtomicUsize::new(0)),
                max_text_bytes: config.max_output_bytes,
            };
            let execution = Execution {
                id: id.clone(),
                skill,
                input: request.input,
                options: request.options,
                events: progress,
                max_output_bytes: config.max_output_bytes,
            };
            let started = tokio::time::Instant::now();
            let work = async {
                sender
                    .send(RunEvent::Started {
                        execution_id: id.clone(),
                        skill:        execution.skill.id.clone(),
                    })
                    .await
                    .map_err(|_| AgentError::Cancelled)?;
                backend.execute(execution).await
            };
            // Surround the *whole* backend future, including HTTP startup,
            // polls and progress sends. Backpressure cannot disable the timer.
            let result = tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(AgentError::Cancelled),
                () = tokio::time::sleep(Duration::from_secs(config.run_timeout_seconds)) => Err(AgentError::Timeout),
                result = AssertUnwindSafe(work).catch_unwind() => result.unwrap_or(Err(AgentError::Protocol("backend task panicked"))),
            };
            let outcome = match result {
                Ok(completion) if completion.text.len() <= config.max_output_bytes => {
                    RunEvent::Completed {
                        execution_id: id.clone(),
                        text:         completion.text,
                    }
                }
                Ok(_) => error_event(&id, AgentError::OutputLimit),
                Err(AgentError::Cancelled) => RunEvent::Cancelled {
                    execution_id: id.clone(),
                },
                Err(error) => error_event(&id, error),
            };
            tracing::info!(execution_id = %id, status = outcome.event_name(), latency_ms = started.elapsed().as_millis(), "agent execution finished");
            finished.send_replace(Some(Arc::new(outcome)));
            // Closing progress wakes recv(), which drains progress before
            // yielding exactly one terminal event from the independent channel.
            drop(sender);
        });
        Ok(handle)
    }
}

impl<B: Backend> Service<RunRequest> for AgentService<B> {
    type Response = RunHandle;
    type Error = AgentError;

    async fn call(&self, request: RunRequest) -> Result<RunHandle, AgentError> {
        self.start(request)
    }
}

fn error_event(id: &str, error: AgentError) -> RunEvent {
    RunEvent::Error {
        execution_id: id.into(),
        code:         error.code().into(),
        message:      error.to_string(),
    }
}
