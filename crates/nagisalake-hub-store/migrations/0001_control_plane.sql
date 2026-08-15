-- All identifiers are text UUIDs so the repository can share IDs with the
-- protocol without compile-time database type coupling. Every resource table
-- carries organization_id and uses composite ownership constraints.
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    email_normalized TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    email_verified_at BIGINT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS organizations (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS memberships (
    organization_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'operator', 'member', 'viewer')),
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, user_id),
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS browser_sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    access_token_hash TEXT NOT NULL UNIQUE,
    refresh_token_hash TEXT NOT NULL UNIQUE,
    csrf_token_hash TEXT NOT NULL,
    family_id TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    last_seen_at BIGINT NOT NULL,
    access_expires_at BIGINT NOT NULL,
    refresh_expires_at BIGINT NOT NULL,
    revoked_at BIGINT,
    user_agent_hash TEXT,
    ip_hash TEXT,
    FOREIGN KEY (organization_id, user_id)
        REFERENCES memberships(organization_id, user_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS api_keys (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    creator_user_id TEXT NOT NULL,
    name TEXT NOT NULL,
    prefix TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    scopes TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    last_used_at BIGINT,
    expires_at BIGINT,
    revoked_at BIGINT,
    FOREIGN KEY (organization_id, creator_user_id)
        REFERENCES memberships(organization_id, user_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS worker_credentials (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    name TEXT NOT NULL,
    token_prefix TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    allowed_namespace TEXT,
    created_at BIGINT NOT NULL,
    last_used_at BIGINT,
    expires_at BIGINT,
    revoked_at BIGINT,
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS workers (
    organization_id TEXT NOT NULL,
    id TEXT NOT NULL,
    owner_user_id TEXT,
    namespace TEXT NOT NULL,
    node_name TEXT NOT NULL,
    worker_version TEXT NOT NULL,
    capabilities_json TEXT NOT NULL,
    last_session_id TEXT,
    last_seen_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, id),
    UNIQUE (organization_id, namespace, node_name),
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS workflow_versions (
    organization_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    version TEXT NOT NULL,
    manifest_json TEXT,
    output_types_json TEXT NOT NULL,
    content_hash TEXT,
    visibility TEXT NOT NULL DEFAULT 'private',
    approval_state TEXT NOT NULL DEFAULT 'approved',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, workflow_id, version),
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS artifacts (
    organization_id TEXT NOT NULL,
    id TEXT NOT NULL,
    job_id TEXT,
    name TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    sha256 TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending_upload', 'ready')),
    object_key TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, id),
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS artifact_upload_requests (
    organization_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    job_id TEXT,
    attempt BIGINT,
    created_at BIGINT NOT NULL,
    completed_at BIGINT,
    PRIMARY KEY (organization_id, request_id),
    FOREIGN KEY (organization_id, artifact_id)
        REFERENCES artifacts(organization_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS jobs (
    organization_id TEXT NOT NULL,
    id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    actor_kind TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    parameters_json TEXT NOT NULL,
    input_artifact_ids_json TEXT NOT NULL,
    output_artifact_ids_json TEXT NOT NULL,
    worker_id TEXT NOT NULL,
    worker_organization_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    attempt BIGINT NOT NULL,
    state TEXT NOT NULL,
    progress REAL,
    prompt_id TEXT,
    error TEXT,
    last_event BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, id),
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS job_events (
    organization_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    attempt BIGINT NOT NULL,
    sequence BIGINT NOT NULL,
    kind TEXT NOT NULL,
    progress REAL,
    prompt_id TEXT,
    message TEXT NOT NULL,
    unix_ms BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, job_id, attempt, sequence),
    FOREIGN KEY (organization_id, job_id)
        REFERENCES jobs(organization_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS idempotency_records (
    organization_id TEXT NOT NULL,
    actor_kind TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    job_id TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, actor_kind, actor_id, endpoint, idempotency_key),
    FOREIGN KEY (organization_id, job_id)
        REFERENCES jobs(organization_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS dispatch_outbox (
    organization_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    attempt BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts BIGINT NOT NULL DEFAULT 0,
    available_at BIGINT NOT NULL,
    claimed_at BIGINT,
    last_error TEXT,
    PRIMARY KEY (organization_id, job_id, attempt),
    FOREIGN KEY (organization_id, job_id)
        REFERENCES jobs(organization_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS quota_policies (
    organization_id TEXT PRIMARY KEY,
    max_concurrent_jobs BIGINT NOT NULL DEFAULT 1,
    max_storage_bytes BIGINT NOT NULL DEFAULT 10737418240,
    max_jobs_per_period BIGINT NOT NULL DEFAULT 1000,
    period_seconds BIGINT NOT NULL DEFAULT 2592000,
    updated_at BIGINT NOT NULL,
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS quota_usage (
    organization_id TEXT PRIMARY KEY,
    active_jobs BIGINT NOT NULL DEFAULT 0,
    storage_bytes BIGINT NOT NULL DEFAULT 0,
    period_jobs BIGINT NOT NULL DEFAULT 0,
    period_started_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS usage_ledger (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    actor_id TEXT,
    job_id TEXT,
    metric TEXT NOT NULL,
    amount BIGINT NOT NULL,
    idempotency_key TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE (organization_id, metric, idempotency_key),
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS audit_logs (
    id TEXT PRIMARY KEY,
    organization_id TEXT,
    actor_id TEXT,
    actor_kind TEXT,
    request_id TEXT,
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT,
    outcome TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_jobs_org_updated ON jobs(organization_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_artifacts_org_created ON artifacts(organization_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_org_created ON audit_logs(organization_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_dispatch_pending ON dispatch_outbox(status, available_at);

CREATE TABLE IF NOT EXISTS device_share_invites (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    code_prefix TEXT NOT NULL,
    code_hash TEXT NOT NULL UNIQUE,
    max_uses BIGINT NOT NULL DEFAULT 1,
    use_count BIGINT NOT NULL DEFAULT 0,
    expires_at BIGINT,
    revoked_at BIGINT,
    created_at BIGINT NOT NULL,
    FOREIGN KEY (organization_id, device_id)
        REFERENCES workers(organization_id, id) ON DELETE CASCADE,
    FOREIGN KEY (organization_id, owner_user_id)
        REFERENCES memberships(organization_id, user_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS device_grants (
    id TEXT PRIMARY KEY,
    device_organization_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    grantee_user_id TEXT NOT NULL,
    invite_id TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    revoked_at BIGINT,
    UNIQUE (device_organization_id, device_id, grantee_user_id),
    FOREIGN KEY (device_organization_id, device_id)
        REFERENCES workers(organization_id, id) ON DELETE CASCADE,
    FOREIGN KEY (device_organization_id, owner_user_id)
        REFERENCES memberships(organization_id, user_id) ON DELETE CASCADE,
    FOREIGN KEY (invite_id) REFERENCES device_share_invites(id) ON DELETE RESTRICT,
    FOREIGN KEY (grantee_user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_device_grants_user ON device_grants(grantee_user_id, revoked_at);
