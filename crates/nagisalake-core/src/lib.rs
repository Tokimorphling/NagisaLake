//! Runtime-neutral domain types for distributed ComfyUI execution.

use nagisalake_protocol::{DispatchJob, JobEvent};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Received,
    Accepted,
    Running,
    Uploading,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Queued,
                Self::Queued | Self::Received | Self::Cancelled
            ) | (Self::Received, Self::Received)
                | (Self::Accepted, Self::Accepted)
                | (Self::Running, Self::Running)
                | (Self::Uploading, Self::Uploading)
                | (Self::Completed, Self::Completed)
                | (Self::Failed, Self::Failed)
                | (Self::Cancelled, Self::Cancelled)
        ) || matches!(
            (self, next),
            (
                Self::Received,
                Self::Accepted | Self::Failed | Self::Cancelled
            ) | (
                Self::Accepted,
                Self::Running | Self::Failed | Self::Cancelled
            ) | (
                Self::Running,
                Self::Uploading | Self::Failed | Self::Cancelled
            ) | (Self::Uploading, Self::Completed | Self::Failed)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub dispatch:       DispatchJob,
    pub state:          JobState,
    pub prompt_id:      Option<String>,
    pub event_sequence: u64,
    pub pending_event:  Option<JobEvent>,
}

impl JobRecord {
    pub fn received(dispatch: DispatchJob) -> Self {
        Self {
            dispatch,
            state: JobState::Received,
            prompt_id: None,
            event_sequence: 0,
            pending_event: None,
        }
    }

    pub fn transition(&mut self, next: JobState) -> Result<(), NagisaError> {
        if !self.state.can_transition_to(next) {
            return Err(NagisaError::InvalidStateTransition {
                current: self.state,
                next,
            });
        }
        self.state = next;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ComfyPromptRequest {
    pub job_id:    String,
    pub client_id: String,
    pub workflow:  JsonValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyPromptResponse {
    pub prompt_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyHistoryRequest {
    pub prompt_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComfyHistoryResponse {
    Pending,
    Complete(Vec<OutputRef>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyQueueStatusRequest {
    pub prompt_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComfyPromptStatus {
    Unknown,
    Queued { position: u32 },
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyUploadImageRequest {
    pub path:      PathBuf,
    pub file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyUploadImageResponse {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyQueueDeleteRequest {
    pub prompt_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyViewRequest {
    pub output: OutputRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRef {
    pub filename:     String,
    #[serde(default)]
    pub subfolder:    String,
    #[serde(default = "default_output_storage_type")]
    pub storage_type: String,
    pub content_type: String,
}

fn default_output_storage_type() -> String {
    "output".into()
}

#[derive(Debug, Clone)]
pub struct UpsertDispatch(pub DispatchJob);

#[derive(Debug, Clone)]
pub struct GetJob(pub String);

#[derive(Debug, Clone, Copy)]
pub struct ListUnfinished;

#[derive(Debug, Clone)]
pub struct SetJobState {
    pub job_id: String,
    pub state:  JobState,
}

#[derive(Debug, Clone)]
pub struct SetPromptId {
    pub job_id:    String,
    pub prompt_id: String,
}

#[derive(Debug, Clone)]
pub struct SetPendingEvent {
    pub event: JobEvent,
    pub state: Option<JobState>,
}

#[derive(Debug, Clone)]
pub struct ClearPendingEvent {
    pub job_id:   String,
    pub sequence: u64,
}

#[derive(Debug, Error)]
pub enum NagisaError {
    #[error("invalid job state transition from {current:?} to {next:?}")]
    InvalidStateTransition {
        current: JobState,
        next:    JobState,
    },
    #[error("invalid workflow: {0}")]
    InvalidWorkflow(String),
    #[error("ComfyUI request failed: {0}")]
    ComfyUi(String),
    #[error("artifact operation failed: {0}")]
    Artifact(String),
    #[error("journal operation failed: {0}")]
    Journal(String),
    #[error("transport operation failed: {0}")]
    Transport(String),
    #[error("job was cancelled")]
    Cancelled,
    #[error("operation timed out: {0}")]
    Timeout(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploading_cannot_be_cancelled_after_outputs_exist() {
        assert!(!JobState::Uploading.can_transition_to(JobState::Cancelled));
        assert!(JobState::Uploading.can_transition_to(JobState::Completed));
        assert!(JobState::Uploading.can_transition_to(JobState::Failed));
    }

    #[test]
    fn queued_jobs_can_only_bind_or_cancel() {
        assert!(JobState::Queued.can_transition_to(JobState::Received));
        assert!(JobState::Queued.can_transition_to(JobState::Cancelled));
        assert!(!JobState::Queued.can_transition_to(JobState::Accepted));
        assert!(!JobState::Queued.can_transition_to(JobState::Running));
    }
}
