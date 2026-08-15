//! Versioned control protocol between Nagisalake hubs and ComfyUI workers.
//!
//! The control plane carries JSON metadata only. Artifact bytes move through
//! short-lived presigned requests and never through the Tokilake control stream.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const PROTOCOL_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkerMessage {
    Register(Register),
    Heartbeat(Heartbeat),
    CommandAck(CommandAck),
    JobEvent(JobEvent),
    ArtifactReady(ArtifactReady),
    ArtifactUploaded(ArtifactUploaded),
    Pong(Pong),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum HubMessage {
    Registered(Registered),
    DispatchJob(DispatchJob),
    CancelJob(CancelJob),
    ArtifactUpload(ArtifactUpload),
    JobEventAck(JobEventAck),
    ArtifactUploadedAck(ArtifactUploadedAck),
    Ping(Ping),
    Error(ProtocolError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Register {
    pub protocol_version: u16,
    pub namespace:        String,
    pub node_name:        String,
    pub worker_version:   String,
    pub capabilities:     WorkerCapabilities,
    /// Non-terminal jobs still present in the worker's durable journal.
    ///
    /// A Hub uses this recovery inventory only to send a targeted
    /// [`CancelJob`] for work that it already considers terminal. It closes the
    /// gap where a worker was disconnected when the Hub failed or cancelled a
    /// job, and would otherwise resume that stale entry forever on restart.
    #[serde(default)]
    pub recovery_job_ids: Vec<String>,
}

/// A recovery inventory is sent in a control frame, so keep it bounded even
/// when a damaged local journal contains more records than the worker's normal
/// admission capacity.
pub const MAX_RECOVERY_JOB_IDS: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerCapabilities {
    #[serde(default)]
    pub workflows: Vec<WorkflowCapability>,
    #[serde(default = "default_parallelism", rename = "concurrency")]
    pub parallelism: u16,
    #[serde(default)]
    pub queue_depth: u16,
    #[serde(default)]
    pub supports_queued_job_cancellation: bool,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

const fn default_parallelism() -> u16 {
    1
}

impl Default for WorkerCapabilities {
    fn default() -> Self {
        Self {
            workflows: Vec::new(),
            parallelism: default_parallelism(),
            queue_depth: 0,
            supports_queued_job_cancellation: false,
            labels: BTreeMap::new(),
        }
    }
}

impl WorkerCapabilities {
    /// Maximum number of running and worker-queued jobs the Hub may admit.
    pub const fn total_capacity(&self) -> u32 {
        self.parallelism as u32 + self.queue_depth as u32
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowCapability {
    pub id:           String,
    pub version:      String,
    #[serde(default)]
    pub output_types: Vec<String>,
    #[serde(default)]
    pub manifest:     Option<WorkflowManifest>,
}

/// Consumer-facing description of the allowlisted workflow contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowManifest {
    #[serde(default = "default_manifest_schema_version")]
    pub schema_version: u16,
    #[serde(default)]
    pub display_name:   String,
    #[serde(default)]
    pub description:    Option<String>,
    #[serde(default)]
    pub inputs:         Vec<WorkflowInput>,
    #[serde(default)]
    pub outputs:        Vec<WorkflowOutput>,
    #[serde(default)]
    pub warnings:       Vec<String>,
}

const fn default_manifest_schema_version() -> u16 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowInputKind {
    Parameter,
    Artifact,
}

/// One public input. `pointer` is diagnostic metadata; only allowlisted names
/// accepted by the worker can mutate the template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowInput {
    pub name:         String,
    pub kind:         WorkflowInputKind,
    #[serde(rename = "type")]
    pub value_type:   String,
    #[serde(default)]
    pub content_type: Option<String>,
    pub pointer:      String,
    #[serde(default)]
    pub required:     bool,
    #[serde(default)]
    pub default:      Option<JsonValue>,
    #[serde(default)]
    pub options:      Vec<String>,
    #[serde(default)]
    pub node_id:      Option<String>,
    #[serde(default)]
    pub node_type:    Option<String>,
    #[serde(default)]
    pub field:        Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowOutput {
    pub name:         String,
    pub content_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registered {
    pub worker_id:                  String,
    pub session_id:                 String,
    pub heartbeat_interval_seconds: u64,
    pub server_unix_ms:             i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub session_id:  String,
    pub sequence:    u64,
    pub active_jobs: u16,
    pub queued_jobs: u16,
    pub unix_ms:     i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchJob {
    pub command_id:       String,
    pub job_id:           String,
    pub attempt:          u32,
    pub workflow_id:      String,
    pub workflow_version: String,
    #[serde(default)]
    pub parameters:       JsonValue,
    #[serde(default)]
    pub inputs:           Vec<JobInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobInput {
    pub artifact_id:  String,
    pub name:         String,
    pub content_type: String,
    pub size_bytes:   u64,
    pub sha256:       String,
    pub download:     PresignedRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelJob {
    pub command_id: String,
    pub job_id:     String,
    #[serde(default)]
    pub reason:     String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAck {
    pub command_id: String,
    pub accepted:   bool,
    #[serde(default)]
    pub message:    String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobEventKind {
    Accepted,
    Running,
    Progress,
    Uploading,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobEvent {
    pub job_id:    String,
    pub attempt:   u32,
    pub sequence:  u64,
    pub kind:      JobEventKind,
    #[serde(default)]
    pub progress:  Option<f32>,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub message:   String,
    pub unix_ms:   i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEventAck {
    pub job_id:   String,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReady {
    pub request_id:   String,
    pub job_id:       String,
    pub attempt:      u32,
    pub name:         String,
    pub content_type: String,
    pub size_bytes:   u64,
    pub sha256:       String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactUpload {
    pub request_id:  String,
    pub artifact_id: String,
    pub upload:      PresignedRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactUploaded {
    pub request_id:  String,
    pub artifact_id: String,
    pub job_id:      String,
    pub attempt:     u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactUploadedAck {
    pub request_id:  String,
    pub artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresignedRequest {
    pub method:             String,
    pub url:                String,
    #[serde(default)]
    pub headers:            BTreeMap<String, String>,
    pub expires_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pong {
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code:      String,
    pub message:   String,
    #[serde(default)]
    pub retryable: bool,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn register(namespace: &str, node_name: &str) -> Register {
        Register {
            protocol_version: PROTOCOL_VERSION,
            namespace:        namespace.into(),
            node_name:        node_name.into(),
            worker_version:   "0.1.0".into(),
            capabilities:     WorkerCapabilities {
                workflows: vec![WorkflowCapability {
                    id:           "portrait".into(),
                    version:      "v1".into(),
                    output_types: vec!["image/png".into()],
                    manifest:     None,
                }],
                parallelism: 1,
                queue_depth: 0,
                supports_queued_job_cancellation: false,
                labels: BTreeMap::new(),
            },
            recovery_job_ids: Vec::new(),
        }
    }

    /// Worker ids are built by joining namespace and node_name, so two distinct
    /// identities must never be able to produce the same id. Sanitising would
    /// collapse `a_b` and `a/b` together; validation has to reject instead.
    #[test]
    fn worker_identities_cannot_collide_after_sanitisation() {
        assert!(register("home-gpu", "comfyui-01").validate().is_ok());
        assert!(register("home_gpu", "comfy.ui_01").validate().is_ok());

        // Each of these used to sanitise down to an already-valid identity.
        for (namespace, node_name, why) in [
            ("home-gpu", "comfyui/01", "slash would become an underscore"),
            (
                "home-gpu",
                "comfyui?01",
                "question mark would become an underscore",
            ),
            ("home-gpu", "comfyui 01", "space would become an underscore"),
            ("home/gpu", "comfyui-01", "slash in the namespace"),
            ("home-gpu", "comfyui:01", "colon would become an underscore"),
        ] {
            let error = register(namespace, node_name)
                .validate()
                .expect_err(&format!("{namespace}/{node_name} must be rejected: {why}"));
            assert!(
                error.to_string().contains("ASCII letters"),
                "unexpected error for {namespace}/{node_name}: {error}"
            );
        }

        // Empty and over-long identities stay rejected.
        assert!(register("", "comfyui-01").validate().is_err());
        assert!(register("home-gpu", "").validate().is_err());
        assert!(
            register("home-gpu", &"a".repeat(MAX_IDENTITY_CHARS + 1))
                .validate()
                .is_err()
        );
        assert!(
            register("home-gpu", &"a".repeat(MAX_IDENTITY_CHARS))
                .validate()
                .is_ok()
        );

        // A leading dot would read as a relative path segment.
        assert!(register("home-gpu", ".hidden").validate().is_err());
    }

    #[test]
    fn recovery_inventory_is_bounded_and_has_unique_nonempty_ids() {
        let mut value = register("home-gpu", "comfyui-01");
        value.recovery_job_ids = vec!["job-a".into(), "job-b".into()];
        assert!(value.validate().is_ok());

        value.recovery_job_ids = vec!["job-a".into(), "job-a".into()];
        assert!(value.validate().is_err());

        value.recovery_job_ids = vec![" ".into()];
        assert!(value.validate().is_err());

        value.recovery_job_ids = (0..=MAX_RECOVERY_JOB_IDS)
            .map(|index| format!("job-{index}"))
            .collect();
        assert!(value.validate().is_err());
    }

    fn dispatch() -> DispatchJob {
        DispatchJob {
            command_id:       "command-1".into(),
            job_id:           "job-1".into(),
            attempt:          1,
            workflow_id:      "portrait".into(),
            workflow_version: "v1".into(),
            parameters:       serde_json::json!({"prompt": "hello"}),
            inputs:           vec![JobInput {
                artifact_id:  "input-1".into(),
                name:         "source.png".into(),
                content_type: "image/png".into(),
                size_bytes:   42,
                sha256:       "a".repeat(64),
                download:     PresignedRequest {
                    method:             "GET".into(),
                    url:                "https://objects.example/input".into(),
                    headers:            BTreeMap::new(),
                    expires_at_unix_ms: 100,
                },
            }],
        }
    }

    #[test]
    fn dispatch_round_trips_without_model_or_binary_body() {
        let message = HubMessage::DispatchJob(dispatch());
        let json = serde_json::to_string(&message).expect("serialize");
        assert!(!json.contains("\"model\""));
        assert!(!json.contains("body"));
        assert_eq!(serde_json::from_str::<HubMessage>(&json).unwrap(), message);
        message.validate().unwrap();
    }

    #[test]
    fn rejects_invalid_artifact_hash() {
        let mut message = dispatch();
        message.inputs[0].sha256 = "not-a-hash".into();
        let error = message.validate().unwrap_err();
        assert_eq!(error.field, "input.sha256");
    }

    #[test]
    fn rejects_duplicate_workflow_capabilities() {
        let capability = WorkflowCapability {
            id:           "portrait".into(),
            version:      "v1".into(),
            output_types: vec!["image/png".into()],
            manifest:     None,
        };
        let capabilities = WorkerCapabilities {
            workflows: vec![capability.clone(), capability],
            parallelism: 1,
            queue_depth: 0,
            supports_queued_job_cancellation: false,
            labels: BTreeMap::new(),
        };
        assert!(capabilities.validate().is_err());
    }

    #[test]
    fn worker_capabilities_keep_the_v2_concurrency_wire_field() {
        let capabilities: WorkerCapabilities = serde_json::from_value(serde_json::json!({
            "workflows": [{"id": "portrait", "version": "v1"}],
            "concurrency": 3,
            "labels": {}
        }))
        .unwrap();

        assert_eq!(capabilities.parallelism, 3);
        assert_eq!(capabilities.queue_depth, 0);
        assert!(!capabilities.supports_queued_job_cancellation);
        assert_eq!(capabilities.total_capacity(), 3);

        let encoded = serde_json::to_value(&capabilities).unwrap();
        assert_eq!(encoded["concurrency"], 3);
        assert!(encoded.get("parallelism").is_none());
    }

    #[test]
    fn worker_capabilities_include_queue_capacity() {
        let capabilities = WorkerCapabilities {
            parallelism: 2,
            queue_depth: 8,
            supports_queued_job_cancellation: true,
            ..register("home", "gpu").capabilities
        };

        assert_eq!(capabilities.total_capacity(), 10);
        capabilities.validate().unwrap();
    }
}
