use crate::EventSender;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// Business request. Provider, agent, system prompt and tool permissions are
/// deliberately not caller-controlled.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    pub skill:   String,
    pub input:   String,
    #[serde(default)]
    pub options: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id:      String,
    pub version: String,
}

/// Validated execution passed to a backend; not an HTTP request type.
pub struct Execution {
    pub id:               String,
    pub skill:            Skill,
    pub input:            String,
    pub options:          BTreeMap<String, serde_json::Value>,
    pub events:           EventSender,
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
}

/// Progress only. A backend cannot emit terminal events: the supervising
/// service owns exactly one terminal outcome, even on timeout/backpressure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    StageStarted { call_id: String, name: String },
    StageFinished { call_id: String, name: String },
    StageFailed { call_id: String, name: String },
    TextDelta { text: String },
    Warning { code: String },
}

/// Stable public protocol. No provider session ids, tool inputs, raw tool
/// outputs, internal paths or upstream error bodies cross this boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    Started {
        execution_id: String,
        skill:        String,
    },
    StageStarted {
        call_id: String,
        name:    String,
    },
    StageFinished {
        call_id: String,
        name:    String,
    },
    StageFailed {
        call_id: String,
        name:    String,
    },
    TextDelta {
        text: String,
    },
    Warning {
        code: String,
    },
    Completed {
        execution_id: String,
        text:         String,
    },
    Error {
        execution_id: String,
        code:         String,
        message:      String,
    },
    Cancelled {
        execution_id: String,
    },
}

impl RunEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::StageStarted { .. } => "stage_started",
            Self::StageFinished { .. } => "stage_finished",
            Self::StageFailed { .. } => "stage_failed",
            Self::TextDelta { .. } => "text_delta",
            Self::Warning { .. } => "warning",
            Self::Completed { .. } => "completed",
            Self::Error { .. } => "error",
            Self::Cancelled { .. } => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Error { .. } | Self::Cancelled { .. }
        )
    }
}

impl From<Progress> for RunEvent {
    fn from(value: Progress) -> Self {
        match value {
            Progress::StageStarted { call_id, name } => Self::StageStarted { call_id, name },
            Progress::StageFinished { call_id, name } => Self::StageFinished { call_id, name },
            Progress::StageFailed { call_id, name } => Self::StageFailed { call_id, name },
            Progress::TextDelta { text } => Self::TextDelta { text },
            Progress::Warning { code } => Self::Warning { code },
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunCommand {
    Cancel,
}

/// Errors contain codes/metadata only; do not retain sensitive response bodies
/// or reqwest errors whose URLs can contain credentials or query secrets.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("invalid agent configuration: {0}")]
    Config(&'static str),
    #[error("unsupported skill")]
    UnsupportedSkill,
    #[error("invalid agent request: {0}")]
    InvalidRequest(&'static str),
    #[error("agent concurrency limit reached")]
    Busy,
    #[error("agent execution cancelled")]
    Cancelled,
    #[error("agent execution timed out")]
    Timeout,
    #[error("agent output exceeded the configured limit")]
    OutputLimit,
    #[error("agent upstream {operation} returned HTTP {status}")]
    UpstreamStatus {
        operation: &'static str,
        status:    u16,
    },
    #[error("agent upstream {0} request failed")]
    Transport(&'static str),
    #[error("invalid agent upstream response: {0}")]
    Protocol(&'static str),
    #[error("agent provider reported an execution error")]
    Provider,
}

impl AgentError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config(_) => "configuration_error",
            Self::UnsupportedSkill => "unsupported_skill",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::OutputLimit => "output_limit",
            Self::UpstreamStatus { .. } | Self::Transport(_) => "upstream_error",
            Self::Protocol(_) => "protocol_error",
            Self::Provider => "provider_error",
        }
    }
}
