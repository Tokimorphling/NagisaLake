//! Versioned control messages between Nagisalake hubs and ComfyUI workers.
//!
//! The control plane carries JSON metadata only. Artifact bytes move through
//! short-lived presigned requests and never through the Tokilake control
//! stream.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

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
