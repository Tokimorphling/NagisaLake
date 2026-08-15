// Mirrors the wire shapes emitted by apps/nagisalake-hub/src/product_api.rs.
// Rust enums use #[serde(rename_all = "snake_case")].

export type Role = 'viewer' | 'member' | 'operator' | 'admin' | 'owner'

export type JobState =
  | 'queued'
  | 'received'
  | 'accepted'
  | 'running'
  | 'uploading'
  | 'completed'
  | 'failed'
  | 'cancelled'

export type JobEventKind =
  | 'accepted'
  | 'running'
  | 'progress'
  | 'uploading'
  | 'completed'
  | 'failed'
  | 'cancelled'

export type ArtifactState = 'pending_upload' | 'ready'

export const TERMINAL_JOB_STATES: readonly JobState[] = ['completed', 'failed', 'cancelled']

export const ROLE_RANK: Record<Role, number> = {
  viewer: 0,
  member: 1,
  operator: 2,
  admin: 3,
  owner: 4,
}

export interface PublicSettings {
  registration_enabled: boolean
  password_auth_enabled: boolean
  max_artifact_bytes: number
  authentication: string[]
  oauth_providers: OAuthProvider[]
}

export interface OAuthProvider {
  name: string
  kind: 'google' | 'github' | 'linuxdo' | 'oidc'
}

export interface PublicUser {
  id: string
  email: string
  status: string
  email_verified: boolean
  created_at: number
}

export interface Membership {
  organization_id: string
  user_id: string
  role: Role
  organization_name: string
}

export interface OrganizationMember {
  organization_id: string
  user_id: string
  email: string
  role: Role
  created_at: number
}

export interface OrganizationInvite {
  id: string
  organization_id: string
  inviter_user_id: string
  code_prefix: string
  role: Role
  created_at: number
  expires_at: number
  accepted_at: number | null
  accepted_by_user_id: string | null
  revoked_at: number | null
}

export interface CreatedOrganizationInvite {
  invite: OrganizationInvite
  plaintext: string
}

export interface AuthBody {
  access_token: string
  token_type: string
  access_expires_at: number
  refresh_expires_at: number
  csrf_token: string
  user: PublicUser
  current_organization_id: string
}

export interface MeResponse {
  user: PublicUser
  current_organization_id: string
  memberships: Membership[]
  auth_kind: string
}

export interface ApiKey {
  id: string
  organization_id: string
  creator_user_id: string
  name: string
  prefix: string
  scopes: string[]
  created_at: number
  last_used_at: number | null
  expires_at: number | null
  revoked_at: number | null
}

export interface CreatedApiKey {
  key: ApiKey
  plaintext: string
}

export interface WorkerCredential {
  id: string
  organization_id: string
  owner_user_id: string | null
  name: string
  token_prefix: string
  allowed_namespace: string | null
  created_at: number
  last_used_at: number | null
  expires_at: number | null
  revoked_at: number | null
}

export interface CreatedWorkerCredential {
  credential: WorkerCredential
  plaintext: string
}

export interface DeviceWorkflow {
  id: string
  version: string
  output_types: string[]
}

export type DeviceAccessKind = 'organization_device' | 'shared_pool_device'

export interface DeviceWorkflowRule {
  id: string
  version: string
}

export interface Device {
  device_organization_id: string
  device_id: string
  owner_user_id: string | null
  namespace: string
  node_name: string
  worker_version: string
  access_kind: DeviceAccessKind
  allowed_workflows: DeviceWorkflowRule[]
  max_concurrent_jobs: number | null
  grant_expires_at: number | null
  connected: boolean
  workflows: DeviceWorkflow[]
}

export interface CreatedDeviceInvite {
  invite_id: string
  code: string
  code_prefix: string
  expires_at: number | null
  max_uses: number
  allowed_workflows: DeviceWorkflowRule[]
  max_concurrent_jobs: number | null
  grant_duration_seconds: number | null
}

export interface DeviceGrant {
  id: string
  device_organization_id: string
  device_id: string
  owner_user_id: string
  grantee_user_id: string
  invite_id: string
  created_at: number
  revoked_at: number | null
  allowed_workflows: DeviceWorkflowRule[]
  max_concurrent_jobs: number | null
  expires_at: number | null
}

export type WorkflowInputKind = 'parameter' | 'artifact'

/** Internal fields (pointer, node_id, node_type, field) are stripped by the Hub. */
export interface WorkflowInput {
  name: string
  kind: WorkflowInputKind
  type: string
  content_type: string | null
  required: boolean
  default: unknown
  options: string[]
}

export interface WorkflowOutput {
  name: string
  content_type: string
}

export interface WorkflowManifest {
  schema_version: number
  display_name: string
  description: string | null
  inputs: WorkflowInput[]
  outputs: WorkflowOutput[]
  warnings: string[]
}

/** session_id and labels are stripped by sanitize_workflow_catalog. */
export interface WorkflowWorker {
  organization_id: string
  worker_id: string
  parallelism: number
  queue_depth: number
  active_jobs: number
  queued_jobs: number
  available: boolean
}

export interface Workflow {
  id: string
  version: string
  output_types: string[]
  manifest: WorkflowManifest | null
  manifest_consistent: boolean
  workers: WorkflowWorker[]
  /** Injected by the Hub: true when at least one worker is available. */
  available: boolean
}

export interface ArtifactView {
  id: string
  job_id: string | null
  name: string
  content_type: string
  size_bytes: number
  sha256: string
  state: ArtifactState
}

export interface PresignedRequest {
  method: string
  url: string
  headers: Record<string, string>
  expires_at_unix_ms: number
}

export interface CreateUploadResponse {
  artifact: ArtifactView
  upload: PresignedRequest
}

export interface DownloadResponse {
  artifact: ArtifactView
  download: PresignedRequest
}

export interface GalleryArtifact {
  id: string
  name: string
  content_type: string
  size_bytes: number
  sha256: string
}

export interface GalleryItem {
  id: string
  artifact: GalleryArtifact
  job_id: string
  workflow_id: string
  workflow_version: string
  display_name: string
  parameters: Record<string, unknown>
  media_kind: 'image' | 'video' | 'audio'
  content_url: string
  published_at_unix_ms: number
  /** True only when the current authenticated user may remove this item. */
  can_unpublish: boolean
}

export interface GalleryDownloadResponse {
  download: PresignedRequest
}

export interface JobEvent {
  sequence: number
  kind: JobEventKind
  progress: number | null
  message: string
  unix_ms: number
}

/** A row of GET /jobs. Carries no event timeline — fetch the job for that. */
export interface JobSummary {
  id: string
  workflow_id: string
  workflow_version: string
  parameters: Record<string, unknown>
  input_artifact_ids: string[]
  output_artifact_ids: string[]
  worker_id: string
  session_id: string
  state: JobState
  progress: number | null
  prompt_id: string | null
  error: string | null
  created_at_unix_ms: number
  updated_at_unix_ms: number
}

/** GET /jobs/{id}. Same fields as the list row plus the event timeline. */
export interface Job extends JobSummary {
  events: JobEvent[]
}

/** All cursor-paginated collection endpoints use this bounded envelope. */
export interface Page<T> {
  items: T[]
  next_cursor: string | null
}

export interface JobEventStreamPayload {
  job_id: string
  state: JobState
  progress: number | null
  error: string | null
  event?: JobEvent
}

export interface JobsPage {
  items: JobSummary[]
  next_cursor: string | null
}

export type AuditLogsPage = Page<AuditLog>
export type ApiKeysPage = Page<ApiKey>
export type WorkerCredentialsPage = Page<WorkerCredential>
export type DevicesPage = Page<Device>
export type WorkflowsPage = Page<Workflow>
export type GalleryItemsPage = Page<GalleryItem>

export type JobBatchesPage = Page<JobBatch>

export interface JobBatch {
  id: string
  workflow_id: string
  workflow_version: string
  total: number
  status: string
  counts: BatchJobCounts
  created_at: number
}

export interface BatchJobCounts {
  queued: number
  received: number
  accepted: number
  running: number
  uploading: number
  completed: number
  failed: number
  cancelled: number
}

export interface CreateBatchRequest {
  workflow_id: string
  workflow_version: string
  count: number
  base_parameters: Record<string, unknown>
  items?: Array<{
    index: number
    client_item_id?: string
    parameter_overrides?: Record<string, unknown>
  }>
  shared_input_artifact_ids?: string[]
  device_organization_id: string
  device_id: string
}

export interface CreateBatchResponse {
  id: string
  workflow_id: string
  workflow_version: string
  total: number
  status: string
  counts: BatchJobCounts
  created_at: number
}

export interface QuotaSnapshot {
  organization_id: string
  max_concurrent_jobs: number
  max_storage_bytes: number
  max_jobs_per_period: number
  period_seconds: number
  active_jobs: number
  storage_bytes: number
  period_jobs: number
  period_started_at: number
}

export interface AuditLog {
  id: string
  organization_id: string | null
  actor_id: string | null
  actor_kind: string | null
  request_id: string | null
  action: string
  resource_type: string
  resource_id: string | null
  outcome: string
  metadata_json: string
  created_at: number
}

export type ApiErrorCode =
  | 'unauthorized'
  | 'forbidden'
  | 'not_found'
  | 'invalid_request'
  | 'conflict'
  | 'quota_exceeded'
  | 'unavailable'
  | 'upstream_error'
  | 'internal_error'

export interface ApiErrorBody {
  error: {
    code: ApiErrorCode
    message: string
    request_id: string
  }
}

export const API_KEY_SCOPES = [
  'workflows:read',
  'workflows:write',
  'jobs:read',
  'jobs:write',
  'jobs:cancel',
  'artifacts:read',
  'artifacts:write',
  'workers:manage',
  'members:manage',
  'api_keys:manage',
  'quota:read',
  'quota:manage',
  'audit:read',
  'devices:read',
  'devices:use',
  'devices:register',
  'devices:share',
] as const

export type ApiKeyScope = (typeof API_KEY_SCOPES)[number]
