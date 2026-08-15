import { api, openAuthenticatedStream, request } from './client'
import type {
  ApiKeysPage,
  AuditLogsPage,
  AuthBody,
  CreateBatchRequest,
  CreateBatchResponse,
  CreateUploadResponse,
  CreatedApiKey,
  CreatedDeviceInvite,
  CreatedWorkerCredential,
  DevicesPage,
  DeviceGrant,
  DeviceWorkflowRule,
  DownloadResponse,
  GalleryDownloadResponse,
  GalleryItem,
  GalleryItemsPage,
  Job,
  JobBatch,
  JobBatchesPage,
  JobsPage,
  MeResponse,
  Membership,
  CreatedOrganizationInvite,
  OrganizationInvite,
  OAuthProvider,
  OrganizationMember,
  PublicSettings,
  QuotaSnapshot,
  Role,
  WorkflowsPage,
  WorkerCredentialsPage,
} from './types'

type PageParams = { limit?: number; cursor?: string }

function pageSuffix(params?: PageParams): string {
  const query = new URLSearchParams()
  if (params?.limit !== undefined) query.set('limit', String(params.limit))
  if (params?.cursor) query.set('cursor', params.cursor)
  return query.size > 0 ? `?${query}` : ''
}

export const endpoints = {
  publicSettings: () => request<PublicSettings>('/settings/public', { anonymous: true }),
  oauthProviders: () =>
    request<{ providers: OAuthProvider[] }>('/auth/oauth/providers', { anonymous: true }),

  register: (body: { email: string; password: string; organization_name?: string }) =>
    request<AuthBody>('/auth/register', { method: 'POST', body, anonymous: true }),

  login: (body: { email: string; password: string; organization_id?: string }) =>
    request<AuthBody>('/auth/login', { method: 'POST', body, anonymous: true }),

  logout: () => api.post<void>('/auth/logout'),
  revokeAllSessions: () => api.post<void>('/auth/revoke-all-sessions'),
  me: () => api.get<MeResponse>('/auth/me'),
  deleteAccount: () => api.delete<void>('/auth/me'),

  organizations: () => api.get<Membership[]>('/organizations'),
  createOrganization: (name: string) => api.post<Membership>('/organizations', { name }),

  members: (org: string) => api.get<OrganizationMember[]>(`/organizations/${org}/members`),
  changeMemberRole: (org: string, userId: string, role: Role) =>
    api.patch<void>(`/organizations/${org}/members/${userId}`, { role }),
  removeMember: (org: string, userId: string) =>
    api.delete<void>(`/organizations/${org}/members/${userId}`),
  organizationInvites: (org: string) =>
    api.get<OrganizationInvite[]>(`/organizations/${org}/member-invites`),
  createOrganizationInvite: (org: string, body: { role: Role; expires_in_seconds?: number }) =>
    api.post<CreatedOrganizationInvite>(`/organizations/${org}/member-invites`, body),
  revokeOrganizationInvite: (org: string, inviteId: string) =>
    api.delete<void>(`/organizations/${org}/member-invites/${inviteId}`),
  acceptOrganizationInvite: (code: string) =>
    api.post<Membership>('/organization-invitations/accept', { code }),
  transferOrganizationOwner: (org: string, userId: string) =>
    api.post<void>(`/organizations/${org}/owner-transfer`, { user_id: userId }),
  exportOrganization: (org: string) =>
    api.get<Record<string, unknown>>(`/organizations/${org}/export`),
  deleteOrganization: (org: string, confirm: string) =>
    request<void>(`/organizations/${org}`, { method: 'DELETE', body: { confirm } }),

  quota: (org: string) => api.get<QuotaSnapshot>(`/organizations/${org}/quota`),
  updateQuota: (
    org: string,
    body: {
      max_concurrent_jobs: number
      max_storage_bytes: number
      max_jobs_per_period: number
      period_seconds: number
    },
  ) => api.patch<QuotaSnapshot>(`/organizations/${org}/quota`, body),
  auditLogs: (org: string, params?: PageParams) =>
    api.get<AuditLogsPage>(`/organizations/${org}/audit-logs${pageSuffix(params)}`),

  apiKeys: (org: string, params?: PageParams) =>
    api.get<ApiKeysPage>(`/organizations/${org}/api-keys${pageSuffix(params)}`),
  createApiKey: (
    org: string,
    body: { name: string; scopes: string[]; expires_in_seconds?: number },
  ) => api.post<CreatedApiKey>(`/organizations/${org}/api-keys`, body),
  revokeApiKey: (org: string, id: string) =>
    api.delete<void>(`/organizations/${org}/api-keys/${id}`),

  workerCredentials: (org: string, params?: PageParams) =>
    api.get<WorkerCredentialsPage>(
      `/organizations/${org}/worker-credentials${pageSuffix(params)}`,
    ),
  createWorkerCredential: (
    org: string,
    body: { name: string; allowed_namespace?: string; expires_in_seconds?: number },
  ) => api.post<CreatedWorkerCredential>(`/organizations/${org}/worker-credentials`, body),
  revokeWorkerCredential: (org: string, id: string) =>
    api.delete<void>(`/organizations/${org}/worker-credentials/${id}`),

  devices: (params?: PageParams) => api.get<DevicesPage>(`/devices${pageSuffix(params)}`),
  createDeviceInvite: (body: {
    device_organization_id: string
    device_id: string
    max_uses?: number
    expires_in_seconds?: number
    allowed_workflows?: DeviceWorkflowRule[]
    max_concurrent_jobs?: number
    grant_duration_seconds?: number
  }) => api.post<CreatedDeviceInvite>('/device-invites', body),
  revokeDeviceInvite: (id: string) => api.delete<void>(`/device-invites/${id}`),
  acceptDeviceInvite: (code: string) =>
    api.post<DeviceGrant>('/device-invitations/accept', { code }),
  revokeDeviceShare: (body: {
    device_organization_id: string
    device_id: string
    grantee_user_id: string
  }) => api.post<void>('/devices/shares/revoke', body),

  workflows: (params?: PageParams) => api.get<WorkflowsPage>(`/workflows${pageSuffix(params)}`),

  createUpload: (body: {
    name: string
    content_type: string
    size_bytes: number
    sha256: string
  }) => api.post<CreateUploadResponse>('/artifacts/uploads', body),
  completeUpload: (artifactId: string, body: { artifact_id: string; size_bytes: number; sha256: string }) =>
    api.post<CreateUploadResponse>(`/artifacts/uploads/${artifactId}/complete`, body),
  download: (artifactId: string) => api.get<DownloadResponse>(`/artifacts/${artifactId}/download`),
  publishGalleryItem: (artifactId: string) =>
    api.post<GalleryItem>('/gallery/items', { artifact_id: artifactId }),
  galleryItems: (params?: PageParams) =>
    api.get<GalleryItemsPage>(`/gallery/items${pageSuffix(params)}`),
  galleryItem: (itemId: string) =>
    api.get<GalleryItem>(`/gallery/items/${encodeURIComponent(itemId)}`),
  galleryItemDownload: (itemId: string) =>
    api.get<GalleryDownloadResponse>(
      `/gallery/items/${encodeURIComponent(itemId)}/download`,
    ),
  unpublishGalleryItem: (itemId: string) =>
    api.delete<void>(`/gallery/items/${encodeURIComponent(itemId)}`),

  /** One page of jobs, newest first. Rows carry no event timeline. */
  jobs: (params?: PageParams) => {
    const query = new URLSearchParams()
    if (params?.limit !== undefined) query.set('limit', String(params.limit))
    if (params?.cursor) query.set('cursor', params.cursor)
    const suffix = query.size > 0 ? `?${query}` : ''
    return api.get<JobsPage>(`/jobs${suffix}`)
  },
  job: (id: string) => api.get<Job>(`/jobs/${id}`),
  jobEvents: (id: string, after: number, signal: AbortSignal) => {
    const query = new URLSearchParams({ after: String(after) })
    return openAuthenticatedStream(`/jobs/${id}/events?${query}`, { signal })
  },
  submitJob: (
    body: {
      workflow_id: string
      workflow_version: string
      parameters: Record<string, unknown>
      input_artifact_ids: string[]
      device_organization_id?: string
      device_id?: string
    },
    idempotencyKey: string,
  ) => request<Job>('/jobs', { method: 'POST', body, idempotencyKey }),
  cancelJob: (id: string) => api.delete<Job>(`/jobs/${id}`),

  /** One page of job batches, newest first. */
  batches: (params?: PageParams) => {
    const query = new URLSearchParams()
    if (params?.limit !== undefined) query.set('limit', String(params.limit))
    if (params?.cursor) query.set('cursor', params.cursor)
    const suffix = query.size > 0 ? `?${query}` : ''
    return api.get<JobBatchesPage>(`/job-batches${suffix}`)
  },
  batch: (batchId: string) => api.get<JobBatch>(`/job-batches/${batchId}`),
  batchJobs: (batchId: string, params?: PageParams) => {
    const query = new URLSearchParams()
    if (params?.limit !== undefined) query.set('limit', String(params.limit))
    if (params?.cursor) query.set('cursor', params.cursor)
    const suffix = query.size > 0 ? `?${query}` : ''
    return api.get<JobsPage>(`/job-batches/${batchId}/jobs${suffix}`)
  },
  createBatch: (body: CreateBatchRequest, idempotencyKey: string) =>
    request<CreateBatchResponse>('/job-batches', { method: 'POST', body, idempotencyKey }),
  cancelBatch: (batchId: string) => api.delete<JobBatch>(`/job-batches/${batchId}`),
}
