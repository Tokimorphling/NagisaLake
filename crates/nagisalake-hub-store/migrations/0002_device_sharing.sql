ALTER TABLE worker_credentials ADD COLUMN IF NOT EXISTS owner_user_id TEXT;
ALTER TABLE browser_sessions ADD COLUMN IF NOT EXISTS csrf_token_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE workers ADD COLUMN IF NOT EXISTS owner_user_id TEXT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS worker_organization_id TEXT;
UPDATE jobs SET worker_organization_id = organization_id WHERE worker_organization_id IS NULL;
ALTER TABLE jobs ALTER COLUMN worker_organization_id SET DEFAULT '';
UPDATE jobs SET worker_organization_id = '' WHERE worker_organization_id IS NULL;
ALTER TABLE jobs ALTER COLUMN worker_organization_id SET NOT NULL;
