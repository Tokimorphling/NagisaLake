ALTER TABLE jobs
    ADD COLUMN IF NOT EXISTS actor_user_id TEXT;

CREATE INDEX IF NOT EXISTS idx_jobs_organization_actor_user_created
    ON jobs(organization_id, actor_user_id, created_at DESC);
