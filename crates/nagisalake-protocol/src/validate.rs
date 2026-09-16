//! Frame validation for the versioned control protocol.

use crate::messages::{
    ArtifactReady, DispatchJob, HubMessage, JobEvent, JobInput, MAX_RECOVERY_JOB_IDS,
    PROTOCOL_VERSION, PresignedRequest, Register, WorkerCapabilities, WorkerMessage,
    WorkflowCapability, WorkflowManifest,
};
use std::collections::BTreeSet;
use thiserror::Error;

pub trait Validate {
    fn validate(&self) -> Result<(), ValidationError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid protocol field {field}: {message}")]
pub struct ValidationError {
    pub field:   &'static str,
    pub message: String,
}

impl ValidationError {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl Validate for WorkerMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Register(value) => value.validate(),
            Self::Heartbeat(value) => required("session_id", &value.session_id),
            Self::CommandAck(value) => required("command_id", &value.command_id),
            Self::JobEvent(value) => value.validate(),
            Self::ArtifactReady(value) => value.validate(),
            Self::ArtifactUploaded(value) => {
                required("request_id", &value.request_id)?;
                required("artifact_id", &value.artifact_id)?;
                required("job_id", &value.job_id)
            }
            Self::Pong(value) => required("nonce", &value.nonce),
        }
    }
}

impl Validate for HubMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Registered(value) => {
                required("worker_id", &value.worker_id)?;
                required("session_id", &value.session_id)?;
                if value.heartbeat_interval_seconds == 0 {
                    return Err(ValidationError::new(
                        "heartbeat_interval_seconds",
                        "must be greater than zero",
                    ));
                }
                Ok(())
            }
            Self::DispatchJob(value) => value.validate(),
            Self::CancelJob(value) => {
                required("command_id", &value.command_id)?;
                required("job_id", &value.job_id)
            }
            Self::ArtifactUpload(value) => {
                required("request_id", &value.request_id)?;
                required("artifact_id", &value.artifact_id)?;
                value.upload.validate()?;
                if value.upload.method != "PUT" {
                    return Err(ValidationError::new("upload.method", "must be PUT"));
                }
                Ok(())
            }
            Self::JobEventAck(value) => required("job_id", &value.job_id),
            Self::ArtifactUploadedAck(value) => {
                required("request_id", &value.request_id)?;
                required("artifact_id", &value.artifact_id)
            }
            Self::Ping(value) => required("nonce", &value.nonce),
            Self::Error(value) => {
                required("code", &value.code)?;
                required("message", &value.message)
            }
        }
    }
}

impl Validate for Register {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ValidationError::new(
                "protocol_version",
                format!("expected {PROTOCOL_VERSION}, got {}", self.protocol_version),
            ));
        }
        identity_component("namespace", &self.namespace)?;
        identity_component("node_name", &self.node_name)?;
        required("worker_version", &self.worker_version)?;
        if self.recovery_job_ids.len() > MAX_RECOVERY_JOB_IDS {
            return Err(ValidationError::new(
                "recovery_job_ids",
                format!("must contain at most {MAX_RECOVERY_JOB_IDS} entries"),
            ));
        }
        let mut recovery_ids = BTreeSet::new();
        for job_id in &self.recovery_job_ids {
            required("recovery_job_ids", job_id)?;
            if !recovery_ids.insert(job_id) {
                return Err(ValidationError::new(
                    "recovery_job_ids",
                    "contains a duplicate job id",
                ));
            }
        }
        self.capabilities.validate()
    }
}

impl Validate for WorkerCapabilities {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.parallelism == 0 {
            return Err(ValidationError::new(
                "capabilities.concurrency",
                "must be greater than zero",
            ));
        }
        if self.workflows.is_empty() {
            return Err(ValidationError::new(
                "capabilities.workflows",
                "must contain at least one workflow",
            ));
        }
        let mut installed = BTreeSet::new();
        for workflow in &self.workflows {
            workflow.validate()?;
            if !installed.insert((&workflow.id, &workflow.version)) {
                return Err(ValidationError::new(
                    "capabilities.workflows",
                    "contains a duplicate id/version",
                ));
            }
        }
        for (key, value) in &self.labels {
            required("capabilities.labels.key", key)?;
            required("capabilities.labels.value", value)?;
        }
        Ok(())
    }
}

impl Validate for WorkflowCapability {
    fn validate(&self) -> Result<(), ValidationError> {
        required("workflow.id", &self.id)?;
        required("workflow.version", &self.version)?;
        for content_type in &self.output_types {
            required("workflow.output_types", content_type)?;
        }
        if let Some(manifest) = &self.manifest {
            manifest.validate()?;
        }
        Ok(())
    }
}

impl WorkflowManifest {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version == 0 {
            return Err(ValidationError::new(
                "workflow.manifest.schema_version",
                "must be greater than zero",
            ));
        }
        required("workflow.manifest.display_name", &self.display_name)?;
        let mut names = BTreeSet::new();
        for input in &self.inputs {
            required("workflow.manifest.input.name", &input.name)?;
            if !names.insert(&input.name) {
                return Err(ValidationError::new(
                    "workflow.manifest.inputs",
                    "contains a duplicate name",
                ));
            }
            required("workflow.manifest.input.type", &input.value_type)?;
            required("workflow.manifest.input.pointer", &input.pointer)?;
            if let Some(content_type) = &input.content_type {
                required("workflow.manifest.input.content_type", content_type)?;
            }
        }
        let mut outputs = BTreeSet::new();
        for output in &self.outputs {
            required("workflow.manifest.output.name", &output.name)?;
            required(
                "workflow.manifest.output.content_type",
                &output.content_type,
            )?;
            if !outputs.insert(&output.name) {
                return Err(ValidationError::new(
                    "workflow.manifest.outputs",
                    "contains a duplicate name",
                ));
            }
        }
        Ok(())
    }
}

impl Validate for DispatchJob {
    fn validate(&self) -> Result<(), ValidationError> {
        required("command_id", &self.command_id)?;
        required("job_id", &self.job_id)?;
        required("workflow_id", &self.workflow_id)?;
        required("workflow_version", &self.workflow_version)?;
        if !self.parameters.is_object() {
            return Err(ValidationError::new("parameters", "must be a JSON object"));
        }
        for input in &self.inputs {
            input.validate()?;
        }
        Ok(())
    }
}

impl Validate for JobInput {
    fn validate(&self) -> Result<(), ValidationError> {
        required("input.artifact_id", &self.artifact_id)?;
        required("input.name", &self.name)?;
        required("input.content_type", &self.content_type)?;
        sha256("input.sha256", &self.sha256)?;
        self.download.validate()?;
        if self.download.method != "GET" {
            return Err(ValidationError::new("input.download.method", "must be GET"));
        }
        Ok(())
    }
}

impl Validate for JobEvent {
    fn validate(&self) -> Result<(), ValidationError> {
        required("job_id", &self.job_id)?;
        if self.sequence == 0 {
            return Err(ValidationError::new(
                "sequence",
                "must be greater than zero",
            ));
        }
        if let Some(progress) = self.progress
            && (!progress.is_finite() || !(0.0..=1.0).contains(&progress))
        {
            return Err(ValidationError::new(
                "progress",
                "must be finite and between zero and one",
            ));
        }
        Ok(())
    }
}

impl Validate for ArtifactReady {
    fn validate(&self) -> Result<(), ValidationError> {
        required("request_id", &self.request_id)?;
        required("job_id", &self.job_id)?;
        required("name", &self.name)?;
        required("content_type", &self.content_type)?;
        sha256("sha256", &self.sha256)
    }
}

impl Validate for PresignedRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        if !matches!(self.method.as_str(), "GET" | "PUT") {
            return Err(ValidationError::new("method", "must be GET or PUT"));
        }
        required("url", &self.url)?;
        if self.expires_at_unix_ms <= 0 {
            return Err(ValidationError::new(
                "expires_at_unix_ms",
                "must be greater than zero",
            ));
        }
        Ok(())
    }
}

fn required(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        Err(ValidationError::new(field, "must not be empty"))
    } else {
        Ok(())
    }
}

/// Longest accepted `namespace` or `node_name`.
pub const MAX_IDENTITY_CHARS: usize = 64;

/// Validates one half of a worker identity.
///
/// `namespace` and `node_name` are joined into the worker id that keys the
/// session registry and the `workers` table, so the accepted character set has
/// to be closed. Sanitising instead of rejecting would map distinct names onto
/// one id: `a_b` and `a/b` would both become `a_b`, and the two workers would
/// evict each other in a reconnect loop while overwriting the same row.
///
/// Rejecting is the safer half of that trade: it fails loudly at registration
/// with a message the operator can act on, and every already-valid identity
/// keeps working unchanged.
fn identity_component(field: &'static str, value: &str) -> Result<(), ValidationError> {
    required(field, value)?;
    if value.chars().count() > MAX_IDENTITY_CHARS {
        return Err(ValidationError::new(
            field,
            format!("must contain at most {MAX_IDENTITY_CHARS} characters"),
        ));
    }
    if let Some(offender) = value
        .chars()
        .find(|value| !value.is_ascii_alphanumeric() && !matches!(value, '.' | '-' | '_'))
    {
        return Err(ValidationError::new(
            field,
            format!("must contain only ASCII letters, digits, '.', '-' or '_'; found {offender:?}"),
        ));
    }
    // Leading dots would let an identity look like a relative path segment.
    if value.starts_with('.') {
        return Err(ValidationError::new(field, "must not start with '.'"));
    }
    Ok(())
}

fn sha256(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ValidationError::new(
            field,
            "must be a 64-character hexadecimal SHA-256",
        ))
    }
}
