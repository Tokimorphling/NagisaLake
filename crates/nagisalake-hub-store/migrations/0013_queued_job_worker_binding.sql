-- Queued batch children do not have a Worker until the scheduler binds them.
-- Migration 0002 installed an empty-string default for legacy job inserts;
-- dropping NOT NULL in 0012 did not remove that default, so new queued rows
-- silently received a fake worker organization instead of NULL.
ALTER TABLE jobs ALTER COLUMN worker_organization_id DROP DEFAULT;

-- Only queued jobs are unbound by definition. Preserve empty strings on any
-- older, already-dispatched row so this repair cannot change its routing.
UPDATE jobs
SET worker_organization_id = NULL
WHERE state = 'queued' AND worker_organization_id = '';

-- Shared-device batches are queued under the consumer organization, so the
-- scheduler scans ready work globally before matching the batch's exact target
-- device. The 0012 index starts with organization_id and cannot support that
-- access path.
CREATE INDEX IF NOT EXISTS idx_dispatch_queue_global_ready
    ON job_dispatch_queue (available_at, priority DESC, created_at, organization_id, job_id);
