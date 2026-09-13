//! Hub-owned agent policy and execution registry; no provider-specific HTTP.
use crate::{HubError, Principal, now_unix_ms};
use nagisalake_agent::{
    AgentError, AgentService, Backend, RunEvent, RunHandle, RunRequest, RuntimeConfig, Skill,
    opencode::{OpenCode, OpenCodeConfig},
};
use nagisalake_hub_store::{FinishAgentExecution, NewAgentExecution, PgStore};
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(flatten)]
    pub backend:                   OpenCodeConfig,
    #[serde(flatten)]
    pub runtime:                   RuntimeConfig,
    /// Empty by default: discovering a skill upstream does not authorize it.
    #[serde(default)]
    pub allowed_skills:            Vec<String>,
    #[serde(default = "default_version")]
    pub skill_version:             String,
    #[serde(default = "default_org_limit")]
    pub max_runs_per_organization: usize,
}
fn default_version() -> String {
    "1".into()
}
fn default_org_limit() -> usize {
    2
}

impl AgentConfig {
    pub fn validate(&self) -> Result<(), HubError> {
        self.backend.validate().map_err(map_agent_error)?;
        self.runtime.validate().map_err(map_agent_error)?;
        if self.max_runs_per_organization == 0
            || self.max_runs_per_organization > self.runtime.max_concurrent_runs
        {
            return Err(HubError::InvalidConfig(
                "invalid per-organization agent limit".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn build(&self) -> Result<AgentState<OpenCode>, HubError> {
        self.validate()?;
        let backend = OpenCode::new(self.backend.clone()).map_err(map_agent_error)?;
        let skills = self
            .allowed_skills
            .iter()
            .map(|id| Skill {
                id:      id.clone(),
                version: self.skill_version.clone(),
            })
            .collect();
        let service =
            AgentService::new(backend, skills, self.runtime.clone()).map_err(map_agent_error)?;
        Ok(AgentState::new(
            service,
            self.max_runs_per_organization,
            self.runtime.run_timeout_seconds,
        ))
    }
}

struct ActiveRun {
    organization_id: String,
    owner_id:        String,
    cancellation:    CancellationToken,
}
type Runs = Arc<Mutex<HashMap<String, ActiveRun>>>;

#[derive(Clone)]
pub(crate) struct AgentState<B> {
    pub service:     AgentService<B>,
    runs:            Runs,
    max_per_org:     usize,
    timeout_seconds: u64,
}

impl<B: Backend> AgentState<B> {
    pub(crate) fn new(service: AgentService<B>, max_per_org: usize, timeout_seconds: u64) -> Self {
        Self {
            service,
            runs: Runs::default(),
            max_per_org,
            timeout_seconds,
        }
    }

    pub(crate) async fn start(
        &self,
        principal: &Principal,
        request: RunRequest,
        store: Option<PgStore>,
    ) -> Result<RunHandle, HubError> {
        self.service
            .validate_request(&request)
            .map_err(map_agent_error)?;
        let options = serde_json::to_string(&request.options)
            .map_err(|_| HubError::InvalidRequest("invalid agent options".into()))?;
        let input = request.input.clone();
        let skill = self
            .service
            .skills()
            .find(|skill| skill.id == request.skill)
            .expect("validated skill")
            .clone();
        let owner_id = owner_id(principal).to_owned();
        let handle = {
            let mut runs = self
                .runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if runs
                .values()
                .filter(|run| run.organization_id == principal.organization_id)
                .count()
                >= self.max_per_org
            {
                return Err(HubError::QuotaExceeded("agent concurrency".into()));
            }
            // start() does no I/O and does not await while holding this lock.
            let run = self.service.start(request).map_err(map_agent_error)?;
            runs.insert(run.id().to_owned(), ActiveRun {
                organization_id: principal.organization_id.clone(),
                owner_id:        owner_id.clone(),
                cancellation:    run.cancellation_token(),
            });
            run
        };
        let registration = Registration {
            runs: self.runs.clone(),
            id:   handle.id().to_owned(),
        };
        let now = now_unix_ms();
        if let Some(store) = &store {
            store
                .create_agent_execution(NewAgentExecution {
                    organization_id: &principal.organization_id,
                    id: handle.id(),
                    owner_id: &owner_id,
                    actor_id: &principal.actor_id,
                    user_id: principal.user_id.as_deref(),
                    skill: &skill.id,
                    skill_version: &skill.version,
                    input: &input,
                    options_json: &options,
                    now,
                    deadline_at: now
                        .saturating_add((self.timeout_seconds as i64 + 30).saturating_mul(1000)),
                })
                .await?;
        }
        let mut completion = handle.completion();
        let organization_id = principal.organization_id.clone();
        tokio::spawn(async move {
            // Kept until persistence finishes, then removed even on any error.
            let _registration = registration;
            if let Ok(outcome) = completion.wait().await
                && let Some(store) = store
            {
                let (state, output, error_code) = match &*outcome {
                    RunEvent::Completed { text, .. } => ("completed", Some(text.as_str()), None),
                    RunEvent::Error { code, .. } => ("failed", None, Some(code.as_str())),
                    RunEvent::Cancelled { .. } => ("cancelled", None, None),
                    _ => ("failed", None, Some("protocol_error")),
                };
                if let Err(error) = store
                    .finish_agent_execution(FinishAgentExecution {
                        organization_id: &organization_id,
                        id: &_registration.id,
                        state,
                        output,
                        error_code,
                        now: now_unix_ms(),
                    })
                    .await
                {
                    tracing::error!(execution_id = %_registration.id, ?error, "failed to persist agent outcome");
                }
            }
        });
        Ok(handle)
    }

    pub(crate) fn cancel(&self, principal: &Principal, id: &str) -> bool {
        let runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(run) = runs.get(id).filter(|run| {
            run.organization_id == principal.organization_id && run.owner_id == owner_id(principal)
        }) {
            run.cancellation.cancel();
            true
        } else {
            false
        }
    }
}

struct Registration {
    runs: Runs,
    id:   String,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

pub(crate) fn owner_id(principal: &Principal) -> &str {
    principal.user_id.as_deref().unwrap_or(&principal.actor_id)
}

pub(crate) fn map_agent_error(error: AgentError) -> HubError {
    match error {
        AgentError::Config(_) => HubError::InvalidConfig(error.to_string()),
        AgentError::UnsupportedSkill | AgentError::InvalidRequest(_) => {
            HubError::InvalidRequest(error.to_string())
        }
        AgentError::Busy => HubError::QuotaExceeded("agent concurrency".into()),
        _ => HubError::Unavailable(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
