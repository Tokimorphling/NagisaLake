use nagisalake_hub_auth::Role;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id:                String,
    pub email:             String,
    /// `None` for a federated-only account. Absent rather than synthetic so it
    /// cannot be mistaken for a usable credential.
    pub password_hash:     Option<String>,
    pub status:            String,
    pub email_verified_at: Option<i64>,
    pub created_at:        i64,
    pub updated_at:        i64,
}
/// How a federated sign-in resolved to a local account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FederatedOutcome {
    /// The provider identity was already linked.
    Existing,
    /// Linked to a pre-existing local account holding the same verified address.
    Linked,
    /// A new account and organization were created.
    Created { organization_id: String },
}

#[derive(Debug, Clone)]
pub struct FederatedLogin {
    pub user:    User,
    pub outcome: FederatedOutcome,
}

/// A provider linked to an account, for display only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedIdentity {
    pub provider:       String,
    pub email:          Option<String>,
    pub email_verified: bool,
    pub created_at:     i64,
    pub last_login_at:  Option<i64>,
}

/// A pending authorization request, returned once by
/// [`crate::PgStore::consume_oauth_authorization`].
#[derive(Debug, Clone)]
pub struct OauthAuthorization {
    pub provider:      String,
    pub pkce_verifier: String,
    pub redirect_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredAccount {
    pub user:            User,
    pub organization_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id:         String,
    pub name:       String,
    pub status:     String,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Membership {
    pub organization_id:   String,
    pub user_id:           String,
    pub role:              Role,
    pub organization_name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationMember {
    pub organization_id: String,
    pub user_id:         String,
    pub email:           String,
    pub role:            Role,
    pub created_at:      i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationInvite {
    pub id:                  String,
    pub organization_id:     String,
    pub inviter_user_id:     String,
    pub code_prefix:         String,
    pub role:                Role,
    pub created_at:          i64,
    pub expires_at:          i64,
    pub accepted_at:         Option<i64>,
    pub accepted_by_user_id: Option<String>,
    pub revoked_at:          Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserSession {
    pub id:                 String,
    pub user_id:            String,
    pub organization_id:    String,
    pub family_id:          String,
    pub csrf_token_hash:    String,
    pub access_expires_at:  i64,
    pub refresh_expires_at: i64,
    pub revoked_at:         Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id:              String,
    pub organization_id: String,
    pub creator_user_id: String,
    pub name:            String,
    pub prefix:          String,
    pub scopes:          Vec<String>,
    pub created_at:      i64,
    pub last_used_at:    Option<i64>,
    pub expires_at:      Option<i64>,
    pub revoked_at:      Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerCredential {
    pub id:                String,
    pub organization_id:   String,
    pub owner_user_id:     Option<String>,
    pub name:              String,
    pub token_prefix:      String,
    pub allowed_namespace: Option<String>,
    pub created_at:        i64,
    pub last_used_at:      Option<i64>,
    pub expires_at:        Option<i64>,
    pub revoked_at:        Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInvite {
    pub id:                     String,
    pub organization_id:        String,
    pub device_id:              String,
    pub owner_user_id:          String,
    pub code_prefix:            String,
    pub max_uses:               i64,
    pub use_count:              i64,
    pub expires_at:             Option<i64>,
    pub revoked_at:             Option<i64>,
    pub created_at:             i64,
    pub allowed_workflows_json: String,
    pub max_concurrent_jobs:    Option<i64>,
    pub grant_duration_seconds: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceWorkflowRule {
    pub id:      String,
    pub version: String,
}
pub fn device_workflow_allowed(
    rules: &[DeviceWorkflowRule],
    workflow_id: &str,
    workflow_version: &str,
) -> bool {
    rules.is_empty()
        || rules
            .iter()
            .any(|rule| rule.id == workflow_id && rule.version == workflow_version)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceGrant {
    pub id:                     String,
    pub device_organization_id: String,
    pub device_id:              String,
    pub owner_user_id:          String,
    pub grantee_user_id:        String,
    pub invite_id:              String,
    pub created_at:             i64,
    pub revoked_at:             Option<i64>,
    pub allowed_workflows:      Vec<DeviceWorkflowRule>,
    pub max_concurrent_jobs:    Option<i64>,
    pub expires_at:             Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceView {
    pub device_organization_id: String,
    pub device_id:              String,
    pub owner_user_id:          Option<String>,
    pub namespace:              String,
    pub node_name:              String,
    pub worker_version:         String,
    pub capabilities_json:      String,
    pub access_kind:            String,
    pub allowed_workflows:      Vec<DeviceWorkflowRule>,
    pub max_concurrent_jobs:    Option<i64>,
    pub grant_expires_at:       Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAccess {
    pub device_organization_id: String,
    pub device_id:              String,
    pub access_kind:            String,
    pub allowed_workflows:      Vec<DeviceWorkflowRule>,
    pub max_concurrent_jobs:    Option<i64>,
    pub grant_expires_at:       Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredWorkflow {
    pub organization_id:   String,
    pub workflow_id:       String,
    pub version:           String,
    pub manifest_json:     Option<String>,
    pub output_types_json: String,
    pub content_hash:      Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredArtifact {
    pub organization_id: String,
    pub id:              String,
    pub job_id:          Option<String>,
    pub name:            String,
    pub content_type:    String,
    pub size_bytes:      i64,
    pub sha256:          String,
    pub state:           String,
    pub object_key:      String,
    pub created_at:      i64,
    pub updated_at:      i64,
}
/// Immutable source data used by the Hub to build a safe public snapshot.
#[derive(Debug, Clone)]
pub struct GalleryPublishCandidate {
    pub artifact_id:      String,
    pub content_type:     String,
    pub workflow_id:      String,
    pub workflow_version: String,
    pub parameters_json:  String,
    pub manifest_json:    Option<String>,
}
/// Durable gallery metadata. The HTTP layer deliberately does not serialize
/// the organization or owner fields.
#[derive(Debug, Clone)]
pub struct StoredGalleryItem {
    pub id:               String,
    pub organization_id:  String,
    pub artifact_id:      String,
    pub job_id:           String,
    pub owner_user_id:    String,
    pub workflow_id:      String,
    pub workflow_version: String,
    pub display_name:     String,
    pub parameters_json:  String,
    pub published_at:     i64,
    pub artifact_name:    String,
    pub content_type:     String,
    pub size_bytes:       i64,
    pub sha256:           String,
}
#[derive(Debug, Clone)]
pub struct GalleryContent {
    pub artifact_name: String,
    pub content_type:  String,
    pub size_bytes:    i64,
    pub sha256:        String,
    pub object_key:    String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredJob {
    pub organization_id: String,
    pub id: String,
    pub actor_id: String,
    pub actor_kind: String,
    pub actor_user_id: Option<String>,
    pub workflow_id: String,
    pub workflow_version: String,
    pub parameters_json: String,
    pub input_artifact_ids_json: String,
    pub output_artifact_ids_json: String,
    pub worker_id: Option<String>,
    pub worker_organization_id: Option<String>,
    pub session_id: Option<String>,
    pub attempt: i64,
    pub state: String,
    pub progress: Option<f32>,
    pub prompt_id: Option<String>,
    pub error: Option<String>,
    pub last_event: i64,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredJobEvent {
    pub organization_id: String,
    pub job_id:          String,
    pub attempt:         i64,
    pub sequence:        i64,
    pub kind:            String,
    pub progress:        Option<f32>,
    pub prompt_id:       Option<String>,
    pub message:         String,
    pub unix_ms:         i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredUploadRequest {
    pub organization_id: String,
    pub request_id:      String,
    pub artifact_id:     String,
    pub job_id:          Option<String>,
    pub attempt:         Option<i64>,
    pub created_at:      i64,
    pub completed_at:    Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub organization_id:     String,
    pub max_concurrent_jobs: i64,
    pub max_storage_bytes:   i64,
    pub max_jobs_per_period: i64,
    pub period_seconds:      i64,
    pub active_jobs:         i64,
    pub storage_bytes:       i64,
    pub period_jobs:         i64,
    pub period_started_at:   i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchOutbox {
    pub organization_id: String,
    pub job_id:          String,
    pub attempt:         i64,
}
pub struct QuotaPolicyUpdate<'a> {
    pub organization_id:     &'a str,
    pub max_concurrent_jobs: i64,
    pub max_storage_bytes:   i64,
    pub max_jobs_per_period: i64,
    pub period_seconds:      i64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct QuotaReconcileStats {
    pub corrected_organizations: u64,
    pub failed_jobs:             i64,
    pub active_jobs:             i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    pub id:              String,
    pub organization_id: Option<String>,
    pub actor_id:        Option<String>,
    pub actor_kind:      Option<String>,
    pub request_id:      Option<String>,
    pub action:          String,
    pub resource_type:   String,
    pub resource_id:     Option<String>,
    pub outcome:         String,
    pub metadata_json:   String,
    pub created_at:      i64,
}
#[derive(Debug, Clone)]
pub struct StoreSnapshot {
    pub artifacts: Vec<StoredArtifact>,
    pub jobs:      Vec<StoredJob>,
    pub workflows: Vec<StoredWorkflow>,
}

pub struct NewSession<'a> {
    pub id:                 &'a str,
    pub user_id:            &'a str,
    pub organization_id:    &'a str,
    pub access_token_hash:  &'a str,
    pub refresh_token_hash: &'a str,
    pub csrf_token_hash:    &'a str,
    pub family_id:          &'a str,
    pub now:                i64,
    pub access_expires_at:  i64,
    pub refresh_expires_at: i64,
    pub user_agent_hash:    Option<&'a str>,
    pub ip_hash:            Option<&'a str>,
}
pub struct RotateSession<'a> {
    pub session_id: &'a str,
    pub expected_refresh_token_hash: &'a str,
    pub access_token_hash: &'a str,
    pub refresh_token_hash: &'a str,
    pub now: i64,
    pub access_expires_at: i64,
    pub refresh_expires_at: i64,
}
pub struct NewApiKey<'a> {
    pub id:              &'a str,
    pub organization_id: &'a str,
    pub creator_user_id: &'a str,
    pub name:            &'a str,
    pub prefix:          &'a str,
    pub key_hash:        &'a str,
    pub scopes:          &'a str,
    pub created_at:      i64,
    pub expires_at:      Option<i64>,
}
pub struct NewWorkerCredential<'a> {
    pub id:                &'a str,
    pub organization_id:   &'a str,
    pub owner_user_id:     Option<&'a str>,
    pub name:              &'a str,
    pub token_prefix:      &'a str,
    pub token_hash:        &'a str,
    pub allowed_namespace: Option<&'a str>,
    pub created_at:        i64,
    pub expires_at:        Option<i64>,
}
pub struct NewDeviceInvite<'a> {
    pub id:                     &'a str,
    pub organization_id:        &'a str,
    pub device_id:              &'a str,
    pub owner_user_id:          &'a str,
    pub code_prefix:            &'a str,
    pub code_hash:              &'a str,
    pub max_uses:               i64,
    pub expires_at:             Option<i64>,
    pub created_at:             i64,
    pub allowed_workflows_json: &'a str,
    pub max_concurrent_jobs:    Option<i64>,
    pub grant_duration_seconds: Option<i64>,
}
pub struct NewOrganizationInvite<'a> {
    pub id:              &'a str,
    pub organization_id: &'a str,
    pub inviter_user_id: &'a str,
    pub code_prefix:     &'a str,
    pub code_hash:       &'a str,
    pub role:            Role,
    pub created_at:      i64,
    pub expires_at:      i64,
}
pub struct WorkerUpsert<'a> {
    pub organization_id:   &'a str,
    pub id:                &'a str,
    pub owner_user_id:     Option<&'a str>,
    pub namespace:         &'a str,
    pub node_name:         &'a str,
    pub worker_version:    &'a str,
    pub capabilities_json: &'a str,
    pub session_id:        Option<&'a str>,
    pub now:               i64,
}
pub struct WorkflowUpsert<'a> {
    pub organization_id:   &'a str,
    pub worker_id:         &'a str,
    pub workflow_id:       &'a str,
    pub version:           &'a str,
    pub manifest_json:     Option<&'a str>,
    pub output_types_json: &'a str,
    pub content_hash:      Option<&'a str>,
    pub now:               i64,
}
pub struct ArtifactUpsert<'a> {
    pub organization_id: &'a str,
    pub id:              &'a str,
    pub job_id:          Option<&'a str>,
    pub name:            &'a str,
    pub content_type:    &'a str,
    pub size_bytes:      u64,
    pub sha256:          &'a str,
    pub state:           &'a str,
    pub object_key:      &'a str,
    pub now:             i64,
    /// Deadline for a `pending_upload` artifact, after which its reserved quota
    /// is reclaimed. `None` for artifacts that are already `ready`.
    pub expires_at:      Option<i64>,
}
pub struct PublishGalleryItem<'a> {
    pub id:              &'a str,
    pub organization_id: &'a str,
    pub artifact_id:     &'a str,
    pub owner_user_id:   &'a str,
    pub display_name:    &'a str,
    pub parameters_json: &'a str,
    pub published_at:    i64,
}

/// A pending upload whose deadline passed, returned by
/// [`crate::PgStore::reclaim_expired_uploads`] so the caller can delete the object.
#[derive(Debug, Clone)]
pub struct ReclaimedUpload {
    pub organization_id: String,
    pub id:              String,
    pub object_key:      String,
    pub size_bytes:      i64,
}
pub struct UploadRequestUpsert<'a> {
    pub organization_id: &'a str,
    pub request_id:      &'a str,
    pub artifact_id:     &'a str,
    pub job_id:          Option<&'a str>,
    pub attempt:         Option<i64>,
    pub now:             i64,
}
pub struct JobUpsert<'a> {
    pub organization_id: &'a str,
    pub id: &'a str,
    pub actor_id: &'a str,
    pub actor_kind: &'a str,
    pub actor_user_id: Option<&'a str>,
    pub workflow_id: &'a str,
    pub workflow_version: &'a str,
    pub parameters_json: &'a str,
    pub input_artifact_ids_json: &'a str,
    pub output_artifact_ids_json: &'a str,
    pub worker_id: &'a str,
    pub worker_organization_id: &'a str,
    pub session_id: &'a str,
    pub attempt: i64,
    pub state: &'a str,
    pub progress: Option<f32>,
    pub prompt_id: Option<&'a str>,
    pub error: Option<&'a str>,
    pub last_event: i64,
    pub now: i64,
}
pub struct DeviceUseAdmission<'a> {
    pub organization_id:        &'a str,
    pub user_id:                &'a str,
    pub device_organization_id: &'a str,
    pub device_id:              &'a str,
    pub workflow_id:            &'a str,
    pub workflow_version:       &'a str,
    pub requested_jobs:         i64,
    pub now:                    i64,
}
pub struct EventInsert<'a> {
    pub organization_id: &'a str,
    pub job_id:          &'a str,
    pub attempt:         i64,
    pub sequence:        i64,
    pub kind:            &'a str,
    pub progress:        Option<f32>,
    pub prompt_id:       Option<&'a str>,
    pub message:         &'a str,
    pub unix_ms:         i64,
    pub now:             i64,
}
pub struct JobEventUpdate<'a> {
    pub session_id:          &'a str,
    pub expected_session_id: &'a str,
    pub expected_state:      &'a str,
    pub expected_last_event: i64,
    pub state:               &'a str,
    pub error:               Option<&'a str>,
}
pub struct ConditionalJobUpdate<'a> {
    pub organization_id:     &'a str,
    pub id:                  &'a str,
    pub attempt:             i64,
    pub expected_state:      &'a str,
    pub expected_last_event: i64,
    pub state:               Option<&'a str>,
    pub error:               Option<&'a str>,
    pub now:                 i64,
}

pub struct CompleteJobOutputUpload<'a> {
    pub organization_id: &'a str,
    pub request_id:      &'a str,
    pub artifact_id:     &'a str,
    pub job_id:          &'a str,
    pub attempt:         i64,
    pub session_id:      &'a str,
    pub now:             i64,
}
pub struct IdempotencyInsert<'a> {
    pub organization_id: &'a str,
    pub actor_kind:      &'a str,
    pub actor_id:        &'a str,
    pub endpoint:        &'a str,
    pub key:             &'a str,
    pub request_hash:    &'a str,
    pub job_id:          &'a str,
    pub now:             i64,
}
pub struct AuditInsert<'a> {
    pub organization_id: Option<&'a str>,
    pub actor_id:        Option<&'a str>,
    pub actor_kind:      Option<&'a str>,
    pub request_id:      Option<&'a str>,
    pub action:          &'a str,
    pub resource_type:   &'a str,
    pub resource_id:     Option<&'a str>,
    pub outcome:         &'a str,
    pub metadata_json:   &'a str,
    pub created_at:      i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyResult {
    pub request_hash: String,
    pub job_id:       String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitJobResult {
    Created,
    Existing { job_id: String },
}

/// A stored job batch (parent resource).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredJobBatch {
    pub id: String,
    pub organization_id: String,
    pub actor_id: String,
    pub actor_kind: String,
    pub actor_user_id: Option<String>,
    pub workflow_id: String,
    pub workflow_version: String,
    pub workflow_content_digest: Option<String>,
    pub base_parameters_json: String,
    pub variation_spec_json: String,
    pub device_organization_id: Option<String>,
    pub device_id: Option<String>,
    pub total_jobs: i64,
    pub retry_of_batch_id: Option<String>,
    pub cancel_requested_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A summary of a job suitable for batch child listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredJobSummary {
    pub id:               String,
    pub batch_id:         Option<String>,
    pub batch_index:      Option<i64>,
    pub state:            String,
    pub progress:         Option<f32>,
    pub workflow_id:      String,
    pub workflow_version: String,
    pub created_at:       i64,
    pub updated_at:       i64,
}

/// State counts for a batch's child jobs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchJobCounts {
    pub queued:    i64,
    pub received:  i64,
    pub accepted:  i64,
    pub running:   i64,
    pub uploading: i64,
    pub completed: i64,
    pub failed:    i64,
    pub cancelled: i64,
}
