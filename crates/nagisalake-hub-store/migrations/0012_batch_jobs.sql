-- Batch jobs: parent resource, Hub dispatch backlog, and shared input artifacts.
--
-- See docs/BATCH_JOBS_COMFYUI_QUEUE_PLAN_CN.md for the full design.
-- This migration is forward-only: existing single jobs remain valid with
-- NULL batch_id and the old artifact ownership column.

-- A Batch is a parent resource over N independent Jobs. It is created in one
-- all-or-nothing transaction; children are still individually queryable,
-- cancellable and retriable.
CREATE TABLE IF NOT EXISTS job_batches (
    id                  TEXT PRIMARY KEY,
    organization_id     TEXT NOT NULL,
    actor_id            TEXT NOT NULL,
    actor_kind          TEXT NOT NULL,
    actor_user_id       TEXT,
    workflow_id         TEXT NOT NULL,
    workflow_version    TEXT NOT NULL,
    workflow_content_digest TEXT,
    base_parameters_json    TEXT NOT NULL,
    variation_spec_json     TEXT NOT NULL DEFAULT '{}',
    device_organization_id  TEXT,
    device_id               TEXT,
    total_jobs          BIGINT NOT NULL,
    retry_of_batch_id   TEXT,
    cancel_requested_at BIGINT,
    created_at          BIGINT NOT NULL,
    updated_at          BIGINT NOT NULL,
    UNIQUE (organization_id, id),
    FOREIGN KEY (organization_id) REFERENCES organizations(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_job_batches_org_created
    ON job_batches (organization_id, created_at DESC, id DESC);

-- Separate idempotency for batch creation (existing idempotency_records
-- can only reference a single job_id).
CREATE TABLE IF NOT EXISTS job_batch_idempotency_records (
    organization_id TEXT NOT NULL,
    actor_kind      TEXT NOT NULL,
    actor_id        TEXT NOT NULL,
    endpoint        TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash    TEXT NOT NULL,
    batch_id        TEXT NOT NULL,
    created_at      BIGINT NOT NULL,
    PRIMARY KEY (organization_id, actor_kind, actor_id, endpoint, idempotency_key),
    FOREIGN KEY (organization_id, batch_id)
        REFERENCES job_batches (organization_id, id) ON DELETE CASCADE
);

-- Hub dispatch backlog: jobs accepted by quota but not yet bound to a Worker.
-- This is distinct from dispatch_outbox, which carries messages to a *bound*
-- Worker. job_dispatch_queue lives before Worker selection.
CREATE TABLE IF NOT EXISTS job_dispatch_queue (
    organization_id TEXT NOT NULL,
    job_id          TEXT NOT NULL,
    available_at    BIGINT NOT NULL,
    priority        BIGINT NOT NULL DEFAULT 0,
    lease_owner     TEXT,
    lease_until     BIGINT,
    created_at      BIGINT NOT NULL,
    PRIMARY KEY (organization_id, job_id),
    FOREIGN KEY (organization_id, job_id)
        REFERENCES jobs (organization_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_dispatch_queue_ready
    ON job_dispatch_queue (organization_id, available_at, priority, created_at);

-- Shared input artifacts: decouples artifact ownership from single-job
-- consumption. artifacts.job_id (renamed conceptually to producer_job_id)
-- still records which job *produced* an output; this table records which
-- jobs *consume* an input.
CREATE TABLE IF NOT EXISTS job_input_artifacts (
    organization_id TEXT NOT NULL,
    job_id          TEXT NOT NULL,
    artifact_id     TEXT NOT NULL,
    input_index     BIGINT NOT NULL,
    input_name      TEXT,
    PRIMARY KEY (organization_id, job_id, input_index),
    FOREIGN KEY (organization_id, job_id)
        REFERENCES jobs (organization_id, id) ON DELETE CASCADE,
    FOREIGN KEY (organization_id, artifact_id)
        REFERENCES artifacts (organization_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_job_input_artifacts_artifact
    ON job_input_artifacts (organization_id, artifact_id);

-- Jobs: add batch and queue metadata. All nullable so existing single jobs
-- continue to work unchanged.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS batch_id TEXT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS batch_index BIGINT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS client_item_id TEXT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS retry_of_job_id TEXT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS queued_at BIGINT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS dispatch_deadline_at BIGINT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS execution_phase TEXT;

-- Worker/session fields become nullable: queued jobs have no Worker yet.
ALTER TABLE jobs ALTER COLUMN worker_id DROP NOT NULL;
ALTER TABLE jobs ALTER COLUMN worker_organization_id DROP NOT NULL;
ALTER TABLE jobs ALTER COLUMN session_id DROP NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_batch_index
    ON jobs (organization_id, batch_id, batch_index)
    WHERE batch_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_batch_client_item
    ON jobs (organization_id, batch_id, client_item_id)
    WHERE batch_id IS NOT NULL AND client_item_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_jobs_state_created
    ON jobs (organization_id, state, created_at, id);

-- Quota: per-batch hard cap.
ALTER TABLE quota_policies ADD COLUMN IF NOT EXISTS max_batch_jobs BIGINT NOT NULL DEFAULT 100;
