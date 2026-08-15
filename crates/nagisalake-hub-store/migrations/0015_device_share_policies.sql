-- Device sharing is a scheduling policy, not just a revocable edge.
-- Empty workflow lists mean "any workflow this device offers" so existing
-- invites and grants keep their previous behavior.
ALTER TABLE device_share_invites
    ADD COLUMN IF NOT EXISTS allowed_workflows_json TEXT NOT NULL DEFAULT '[]',
    ADD COLUMN IF NOT EXISTS max_concurrent_jobs BIGINT,
    ADD COLUMN IF NOT EXISTS grant_duration_seconds BIGINT;

ALTER TABLE device_grants
    ADD COLUMN IF NOT EXISTS allowed_workflows_json TEXT NOT NULL DEFAULT '[]',
    ADD COLUMN IF NOT EXISTS max_concurrent_jobs BIGINT,
    ADD COLUMN IF NOT EXISTS expires_at BIGINT;

CREATE INDEX IF NOT EXISTS idx_device_grants_active_device
    ON device_grants (device_organization_id, device_id, grantee_user_id, revoked_at, expires_at);

CREATE INDEX IF NOT EXISTS idx_jobs_shared_device_active
    ON jobs (organization_id, actor_user_id, worker_organization_id, worker_id)
    WHERE state NOT IN ('completed', 'failed', 'cancelled');

CREATE INDEX IF NOT EXISTS idx_jobs_active_batch_actor
    ON jobs (organization_id, actor_user_id, batch_id)
    WHERE state NOT IN ('completed', 'failed', 'cancelled') AND batch_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_job_batches_target_device
    ON job_batches (organization_id, device_organization_id, device_id, id)
    WHERE device_organization_id IS NOT NULL AND device_id IS NOT NULL;
