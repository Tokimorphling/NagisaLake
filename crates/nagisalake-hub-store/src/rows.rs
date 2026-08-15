use crate::models::*;
use nagisalake_hub_auth::Role;

#[derive(sqlx::FromRow)]
pub(crate) struct UserRow {
    pub(crate) id:                String,
    pub(crate) email:             String,
    pub(crate) password_hash:     Option<String>,
    pub(crate) status:            String,
    pub(crate) email_verified_at: Option<i64>,
    pub(crate) created_at:        i64,
    pub(crate) updated_at:        i64,
}
impl From<UserRow> for User {
    fn from(v: UserRow) -> Self {
        Self {
            id:                v.id,
            email:             v.email,
            password_hash:     v.password_hash,
            status:            v.status,
            email_verified_at: v.email_verified_at,
            created_at:        v.created_at,
            updated_at:        v.updated_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct MembershipRow {
    pub(crate) organization_id: String,
    pub(crate) user_id:         String,
    pub(crate) role:            String,
    pub(crate) name:            String,
}
#[derive(sqlx::FromRow)]
pub(crate) struct OrganizationRow {
    pub(crate) id:         String,
    pub(crate) name:       String,
    pub(crate) status:     String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}
impl From<OrganizationRow> for Organization {
    fn from(value: OrganizationRow) -> Self {
        Self {
            id:         value.id,
            name:       value.name,
            status:     value.status,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}
impl From<MembershipRow> for Membership {
    fn from(v: MembershipRow) -> Self {
        Self {
            organization_id:   v.organization_id,
            user_id:           v.user_id,
            role:              v.role.parse().unwrap_or(Role::Viewer),
            organization_name: v.name,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct OrganizationMemberRow {
    pub(crate) organization_id: String,
    pub(crate) user_id:         String,
    pub(crate) email:           String,
    pub(crate) role:            String,
    pub(crate) created_at:      i64,
}
impl From<OrganizationMemberRow> for OrganizationMember {
    fn from(v: OrganizationMemberRow) -> Self {
        Self {
            organization_id: v.organization_id,
            user_id:         v.user_id,
            email:           v.email,
            role:            v.role.parse().unwrap_or(Role::Viewer),
            created_at:      v.created_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct OrganizationInviteRow {
    pub(crate) id:                  String,
    pub(crate) organization_id:     String,
    pub(crate) inviter_user_id:     String,
    pub(crate) code_prefix:         String,
    pub(crate) role:                String,
    pub(crate) created_at:          i64,
    pub(crate) expires_at:          i64,
    pub(crate) accepted_at:         Option<i64>,
    pub(crate) accepted_by_user_id: Option<String>,
    pub(crate) revoked_at:          Option<i64>,
}
impl From<OrganizationInviteRow> for OrganizationInvite {
    fn from(value: OrganizationInviteRow) -> Self {
        Self {
            id:                  value.id,
            organization_id:     value.organization_id,
            inviter_user_id:     value.inviter_user_id,
            code_prefix:         value.code_prefix,
            role:                value.role.parse().unwrap_or(Role::Viewer),
            created_at:          value.created_at,
            expires_at:          value.expires_at,
            accepted_at:         value.accepted_at,
            accepted_by_user_id: value.accepted_by_user_id,
            revoked_at:          value.revoked_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct SessionRow {
    pub(crate) id:                 String,
    pub(crate) user_id:            String,
    pub(crate) organization_id:    String,
    pub(crate) family_id:          String,
    pub(crate) csrf_token_hash:    String,
    pub(crate) access_expires_at:  i64,
    pub(crate) refresh_expires_at: i64,
    pub(crate) revoked_at:         Option<i64>,
}
impl From<SessionRow> for BrowserSession {
    fn from(v: SessionRow) -> Self {
        Self {
            id:                 v.id,
            user_id:            v.user_id,
            organization_id:    v.organization_id,
            family_id:          v.family_id,
            csrf_token_hash:    v.csrf_token_hash,
            access_expires_at:  v.access_expires_at,
            refresh_expires_at: v.refresh_expires_at,
            revoked_at:         v.revoked_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct ApiKeyRow {
    pub(crate) id:              String,
    pub(crate) organization_id: String,
    pub(crate) creator_user_id: String,
    pub(crate) name:            String,
    pub(crate) prefix:          String,
    pub(crate) scopes:          String,
    pub(crate) created_at:      i64,
    pub(crate) last_used_at:    Option<i64>,
    pub(crate) expires_at:      Option<i64>,
    pub(crate) revoked_at:      Option<i64>,
}
impl From<ApiKeyRow> for ApiKey {
    fn from(v: ApiKeyRow) -> Self {
        Self {
            id:              v.id,
            organization_id: v.organization_id,
            creator_user_id: v.creator_user_id,
            name:            v.name,
            prefix:          v.prefix,
            scopes:          serde_json::from_str(&v.scopes).unwrap_or_default(),
            created_at:      v.created_at,
            last_used_at:    v.last_used_at,
            expires_at:      v.expires_at,
            revoked_at:      v.revoked_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct WorkerCredentialRow {
    pub(crate) id:                String,
    pub(crate) organization_id:   String,
    pub(crate) owner_user_id:     Option<String>,
    pub(crate) name:              String,
    pub(crate) token_prefix:      String,
    pub(crate) allowed_namespace: Option<String>,
    pub(crate) created_at:        i64,
    pub(crate) last_used_at:      Option<i64>,
    pub(crate) expires_at:        Option<i64>,
    pub(crate) revoked_at:        Option<i64>,
}
impl From<WorkerCredentialRow> for WorkerCredential {
    fn from(v: WorkerCredentialRow) -> Self {
        Self {
            id:                v.id,
            organization_id:   v.organization_id,
            owner_user_id:     v.owner_user_id,
            name:              v.name,
            token_prefix:      v.token_prefix,
            allowed_namespace: v.allowed_namespace,
            created_at:        v.created_at,
            last_used_at:      v.last_used_at,
            expires_at:        v.expires_at,
            revoked_at:        v.revoked_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct DeviceInviteRow {
    pub(crate) id:                     String,
    pub(crate) organization_id:        String,
    pub(crate) device_id:              String,
    pub(crate) owner_user_id:          String,
    pub(crate) code_prefix:            String,
    pub(crate) max_uses:               i64,
    pub(crate) use_count:              i64,
    pub(crate) expires_at:             Option<i64>,
    pub(crate) revoked_at:             Option<i64>,
    pub(crate) created_at:             i64,
    pub(crate) allowed_workflows_json: String,
    pub(crate) max_concurrent_jobs:    Option<i64>,
    pub(crate) grant_duration_seconds: Option<i64>,
}
#[derive(Clone, sqlx::FromRow)]
pub(crate) struct DeviceGrantRow {
    pub(crate) id:                     String,
    pub(crate) device_organization_id: String,
    pub(crate) device_id:              String,
    pub(crate) owner_user_id:          String,
    pub(crate) grantee_user_id:        String,
    pub(crate) invite_id:              String,
    pub(crate) created_at:             i64,
    pub(crate) revoked_at:             Option<i64>,
    pub(crate) allowed_workflows_json: String,
    pub(crate) max_concurrent_jobs:    Option<i64>,
    pub(crate) expires_at:             Option<i64>,
}
impl From<DeviceGrantRow> for DeviceGrant {
    fn from(v: DeviceGrantRow) -> Self {
        Self {
            id:                     v.id,
            device_organization_id: v.device_organization_id,
            device_id:              v.device_id,
            owner_user_id:          v.owner_user_id,
            grantee_user_id:        v.grantee_user_id,
            invite_id:              v.invite_id,
            created_at:             v.created_at,
            revoked_at:             v.revoked_at,
            allowed_workflows:      parse_workflow_rules(&v.allowed_workflows_json),
            max_concurrent_jobs:    v.max_concurrent_jobs,
            expires_at:             v.expires_at,
        }
    }
}
impl From<DeviceInviteRow> for DeviceInvite {
    fn from(v: DeviceInviteRow) -> Self {
        Self {
            id:                     v.id,
            organization_id:        v.organization_id,
            device_id:              v.device_id,
            owner_user_id:          v.owner_user_id,
            code_prefix:            v.code_prefix,
            max_uses:               v.max_uses,
            use_count:              v.use_count,
            expires_at:             v.expires_at,
            revoked_at:             v.revoked_at,
            created_at:             v.created_at,
            allowed_workflows_json: v.allowed_workflows_json,
            max_concurrent_jobs:    v.max_concurrent_jobs,
            grant_duration_seconds: v.grant_duration_seconds,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct DeviceRow {
    pub(crate) organization_id:        String,
    pub(crate) id:                     String,
    pub(crate) owner_user_id:          Option<String>,
    pub(crate) namespace:              String,
    pub(crate) node_name:              String,
    pub(crate) worker_version:         String,
    pub(crate) capabilities_json:      String,
    pub(crate) access_kind:            String,
    pub(crate) allowed_workflows_json: String,
    pub(crate) max_concurrent_jobs:    Option<i64>,
    pub(crate) grant_expires_at:       Option<i64>,
}
impl From<DeviceRow> for DeviceView {
    fn from(v: DeviceRow) -> Self {
        Self {
            device_organization_id: v.organization_id,
            device_id:              v.id,
            owner_user_id:          v.owner_user_id,
            namespace:              v.namespace,
            node_name:              v.node_name,
            worker_version:         v.worker_version,
            capabilities_json:      v.capabilities_json,
            access_kind:            v.access_kind,
            allowed_workflows:      parse_workflow_rules(&v.allowed_workflows_json),
            max_concurrent_jobs:    v.max_concurrent_jobs,
            grant_expires_at:       v.grant_expires_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct DeviceAccessRow {
    pub(crate) device_organization_id: String,
    pub(crate) device_id:              String,
    pub(crate) access_kind:            String,
    pub(crate) allowed_workflows_json: String,
    pub(crate) max_concurrent_jobs:    Option<i64>,
    pub(crate) grant_expires_at:       Option<i64>,
}
impl From<DeviceAccessRow> for DeviceAccess {
    fn from(v: DeviceAccessRow) -> Self {
        Self {
            device_organization_id: v.device_organization_id,
            device_id:              v.device_id,
            access_kind:            v.access_kind,
            allowed_workflows:      parse_workflow_rules(&v.allowed_workflows_json),
            max_concurrent_jobs:    v.max_concurrent_jobs,
            grant_expires_at:       v.grant_expires_at,
        }
    }
}
pub(crate) fn parse_workflow_rules(value: &str) -> Vec<DeviceWorkflowRule> {
    serde_json::from_str(value).unwrap_or_default()
}
#[derive(sqlx::FromRow)]
pub(crate) struct WorkflowRow {
    pub(crate) organization_id:   String,
    pub(crate) workflow_id:       String,
    pub(crate) version:           String,
    pub(crate) manifest_json:     Option<String>,
    pub(crate) output_types_json: String,
    pub(crate) content_hash:      Option<String>,
}
impl From<WorkflowRow> for StoredWorkflow {
    fn from(v: WorkflowRow) -> Self {
        Self {
            organization_id:   v.organization_id,
            workflow_id:       v.workflow_id,
            version:           v.version,
            manifest_json:     v.manifest_json,
            output_types_json: v.output_types_json,
            content_hash:      v.content_hash,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct ArtifactRow {
    pub(crate) organization_id: String,
    pub(crate) id:              String,
    pub(crate) job_id:          Option<String>,
    pub(crate) name:            String,
    pub(crate) content_type:    String,
    pub(crate) size_bytes:      i64,
    sha256:                     String,
    pub(crate) state:           String,
    pub(crate) object_key:      String,
    pub(crate) created_at:      i64,
    pub(crate) updated_at:      i64,
}
impl From<ArtifactRow> for StoredArtifact {
    fn from(v: ArtifactRow) -> Self {
        Self {
            organization_id: v.organization_id,
            id:              v.id,
            job_id:          v.job_id,
            name:            v.name,
            content_type:    v.content_type,
            size_bytes:      v.size_bytes,
            sha256:          v.sha256,
            state:           v.state,
            object_key:      v.object_key,
            created_at:      v.created_at,
            updated_at:      v.updated_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct GalleryPublishCandidateRow {
    pub(crate) artifact_id:      String,
    pub(crate) content_type:     String,
    pub(crate) workflow_id:      String,
    pub(crate) workflow_version: String,
    pub(crate) parameters_json:  String,
    pub(crate) manifest_json:    Option<String>,
}
impl From<GalleryPublishCandidateRow> for GalleryPublishCandidate {
    fn from(v: GalleryPublishCandidateRow) -> Self {
        Self {
            artifact_id:      v.artifact_id,
            content_type:     v.content_type,
            workflow_id:      v.workflow_id,
            workflow_version: v.workflow_version,
            parameters_json:  v.parameters_json,
            manifest_json:    v.manifest_json,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct GalleryItemRow {
    pub(crate) id:               String,
    pub(crate) organization_id:  String,
    pub(crate) artifact_id:      String,
    pub(crate) job_id:           String,
    pub(crate) owner_user_id:    String,
    pub(crate) workflow_id:      String,
    pub(crate) workflow_version: String,
    pub(crate) display_name:     String,
    pub(crate) parameters_json:  String,
    pub(crate) published_at:     i64,
    pub(crate) artifact_name:    String,
    pub(crate) content_type:     String,
    pub(crate) size_bytes:       i64,
    pub(crate) sha256:           String,
}
impl From<GalleryItemRow> for StoredGalleryItem {
    fn from(v: GalleryItemRow) -> Self {
        Self {
            id:               v.id,
            organization_id:  v.organization_id,
            artifact_id:      v.artifact_id,
            job_id:           v.job_id,
            owner_user_id:    v.owner_user_id,
            workflow_id:      v.workflow_id,
            workflow_version: v.workflow_version,
            display_name:     v.display_name,
            parameters_json:  v.parameters_json,
            published_at:     v.published_at,
            artifact_name:    v.artifact_name,
            content_type:     v.content_type,
            size_bytes:       v.size_bytes,
            sha256:           v.sha256,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct GalleryContentRow {
    pub(crate) artifact_name: String,
    pub(crate) content_type:  String,
    pub(crate) size_bytes:    i64,
    pub(crate) sha256:        String,
    pub(crate) object_key:    String,
}
impl From<GalleryContentRow> for GalleryContent {
    fn from(v: GalleryContentRow) -> Self {
        Self {
            artifact_name: v.artifact_name,
            content_type:  v.content_type,
            size_bytes:    v.size_bytes,
            sha256:        v.sha256,
            object_key:    v.object_key,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct JobRow {
    pub(crate) organization_id: String,
    pub(crate) id: String,
    pub(crate) actor_id: String,
    pub(crate) actor_kind: String,
    pub(crate) actor_user_id: Option<String>,
    pub(crate) workflow_id: String,
    pub(crate) workflow_version: String,
    pub(crate) parameters_json: String,
    pub(crate) input_artifact_ids_json: String,
    pub(crate) output_artifact_ids_json: String,
    pub(crate) worker_id: Option<String>,
    pub(crate) worker_organization_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) attempt: i64,
    pub(crate) state: String,
    pub(crate) progress: Option<f32>,
    pub(crate) prompt_id: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) last_event: i64,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}
impl From<JobRow> for StoredJob {
    fn from(v: JobRow) -> Self {
        Self {
            organization_id: v.organization_id,
            id: v.id,
            actor_id: v.actor_id,
            actor_kind: v.actor_kind,
            actor_user_id: v.actor_user_id,
            workflow_id: v.workflow_id,
            workflow_version: v.workflow_version,
            parameters_json: v.parameters_json,
            input_artifact_ids_json: v.input_artifact_ids_json,
            output_artifact_ids_json: v.output_artifact_ids_json,
            worker_id: v.worker_id,
            worker_organization_id: v.worker_organization_id,
            session_id: v.session_id,
            attempt: v.attempt,
            state: v.state,
            progress: v.progress,
            prompt_id: v.prompt_id,
            error: v.error,
            last_event: v.last_event,
            created_at: v.created_at,
            updated_at: v.updated_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct EventRow {
    pub(crate) organization_id: String,
    pub(crate) job_id:          String,
    pub(crate) attempt:         i64,
    pub(crate) sequence:        i64,
    pub(crate) kind:            String,
    pub(crate) progress:        Option<f32>,
    pub(crate) prompt_id:       Option<String>,
    pub(crate) message:         String,
    pub(crate) unix_ms:         i64,
}
#[derive(sqlx::FromRow)]
pub(crate) struct UploadRequestRow {
    pub(crate) organization_id: String,
    pub(crate) request_id:      String,
    pub(crate) artifact_id:     String,
    pub(crate) job_id:          Option<String>,
    pub(crate) attempt:         Option<i64>,
    pub(crate) created_at:      i64,
    pub(crate) completed_at:    Option<i64>,
}
impl From<UploadRequestRow> for StoredUploadRequest {
    fn from(v: UploadRequestRow) -> Self {
        Self {
            organization_id: v.organization_id,
            request_id:      v.request_id,
            artifact_id:     v.artifact_id,
            job_id:          v.job_id,
            attempt:         v.attempt,
            created_at:      v.created_at,
            completed_at:    v.completed_at,
        }
    }
}
impl From<EventRow> for StoredJobEvent {
    fn from(v: EventRow) -> Self {
        Self {
            organization_id: v.organization_id,
            job_id:          v.job_id,
            attempt:         v.attempt,
            sequence:        v.sequence,
            kind:            v.kind,
            progress:        v.progress,
            prompt_id:       v.prompt_id,
            message:         v.message,
            unix_ms:         v.unix_ms,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct IdempotencyRow {
    pub(crate) request_hash: String,
    pub(crate) job_id:       String,
}
#[derive(sqlx::FromRow)]
pub(crate) struct QuotaRow {
    pub(crate) organization_id:     String,
    pub(crate) max_concurrent_jobs: i64,
    pub(crate) max_storage_bytes:   i64,
    pub(crate) max_jobs_per_period: i64,
    pub(crate) period_seconds:      i64,
    pub(crate) active_jobs:         i64,
    pub(crate) storage_bytes:       i64,
    pub(crate) period_jobs:         i64,
    pub(crate) period_started_at:   i64,
}
impl From<QuotaRow> for QuotaSnapshot {
    fn from(v: QuotaRow) -> Self {
        Self {
            organization_id:     v.organization_id,
            max_concurrent_jobs: v.max_concurrent_jobs,
            max_storage_bytes:   v.max_storage_bytes,
            max_jobs_per_period: v.max_jobs_per_period,
            period_seconds:      v.period_seconds,
            active_jobs:         v.active_jobs,
            storage_bytes:       v.storage_bytes,
            period_jobs:         v.period_jobs,
            period_started_at:   v.period_started_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct AuditRow {
    pub(crate) id:              String,
    pub(crate) organization_id: Option<String>,
    pub(crate) actor_id:        Option<String>,
    pub(crate) actor_kind:      Option<String>,
    pub(crate) request_id:      Option<String>,
    pub(crate) action:          String,
    pub(crate) resource_type:   String,
    pub(crate) resource_id:     Option<String>,
    pub(crate) outcome:         String,
    pub(crate) metadata_json:   String,
    pub(crate) created_at:      i64,
}
impl From<AuditRow> for AuditLog {
    fn from(v: AuditRow) -> Self {
        Self {
            id:              v.id,
            organization_id: v.organization_id,
            actor_id:        v.actor_id,
            actor_kind:      v.actor_kind,
            request_id:      v.request_id,
            action:          v.action,
            resource_type:   v.resource_type,
            resource_id:     v.resource_id,
            outcome:         v.outcome,
            metadata_json:   v.metadata_json,
            created_at:      v.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
#[allow(dead_code)]
pub(crate) struct QuotaPolicyRow {
    pub(crate) organization_id:     String,
    pub(crate) max_concurrent_jobs: i64,
    pub(crate) max_storage_bytes:   i64,
    pub(crate) max_jobs_per_period: i64,
    pub(crate) period_seconds:      i64,
    pub(crate) max_batch_jobs:      i64,
    pub(crate) updated_at:          i64,
}

#[derive(sqlx::FromRow)]
#[allow(dead_code)]
pub(crate) struct QuotaUsageRow {
    pub(crate) organization_id:   String,
    pub(crate) active_jobs:       i64,
    pub(crate) storage_bytes:     i64,
    pub(crate) period_jobs:       i64,
    pub(crate) period_started_at: i64,
    pub(crate) updated_at:        i64,
}

#[derive(sqlx::FromRow)]
pub(crate) struct JobBatchRow {
    pub(crate) id: String,
    pub(crate) organization_id: String,
    pub(crate) actor_id: String,
    pub(crate) actor_kind: String,
    pub(crate) actor_user_id: Option<String>,
    pub(crate) workflow_id: String,
    pub(crate) workflow_version: String,
    pub(crate) workflow_content_digest: Option<String>,
    pub(crate) base_parameters_json: String,
    pub(crate) variation_spec_json: String,
    pub(crate) device_organization_id: Option<String>,
    pub(crate) device_id: Option<String>,
    pub(crate) total_jobs: i64,
    pub(crate) retry_of_batch_id: Option<String>,
    pub(crate) cancel_requested_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

impl From<JobBatchRow> for crate::models::StoredJobBatch {
    fn from(v: JobBatchRow) -> Self {
        Self {
            id: v.id,
            organization_id: v.organization_id,
            actor_id: v.actor_id,
            actor_kind: v.actor_kind,
            actor_user_id: v.actor_user_id,
            workflow_id: v.workflow_id,
            workflow_version: v.workflow_version,
            workflow_content_digest: v.workflow_content_digest,
            base_parameters_json: v.base_parameters_json,
            variation_spec_json: v.variation_spec_json,
            device_organization_id: v.device_organization_id,
            device_id: v.device_id,
            total_jobs: v.total_jobs,
            retry_of_batch_id: v.retry_of_batch_id,
            cancel_requested_at: v.cancel_requested_at,
            created_at: v.created_at,
            updated_at: v.updated_at,
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct JobSummaryRow {
    pub(crate) id:               String,
    pub(crate) batch_id:         Option<String>,
    pub(crate) batch_index:      Option<i64>,
    pub(crate) state:            String,
    pub(crate) progress:         Option<f32>,
    pub(crate) workflow_id:      String,
    pub(crate) workflow_version: String,
    pub(crate) created_at:       i64,
    pub(crate) updated_at:       i64,
}

impl From<JobSummaryRow> for crate::models::StoredJobSummary {
    fn from(v: JobSummaryRow) -> Self {
        Self {
            id:               v.id,
            batch_id:         v.batch_id,
            batch_index:      v.batch_index,
            state:            v.state,
            progress:         v.progress,
            workflow_id:      v.workflow_id,
            workflow_version: v.workflow_version,
            created_at:       v.created_at,
            updated_at:       v.updated_at,
        }
    }
}
