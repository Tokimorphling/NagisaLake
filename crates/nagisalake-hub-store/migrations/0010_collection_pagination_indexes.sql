-- Keep the public collection endpoints keyset-friendly as organizations grow.
-- The id columns are deterministic tie-breakers for same-millisecond writes.
CREATE INDEX IF NOT EXISTS idx_audit_org_created_id
    ON audit_logs (organization_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_api_keys_org_created_id
    ON api_keys (organization_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_api_keys_org_creator_created_id
    ON api_keys (organization_id, creator_user_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_worker_credentials_org_id
    ON worker_credentials (organization_id, id);

CREATE INDEX IF NOT EXISTS idx_worker_credentials_org_owner_id
    ON worker_credentials (organization_id, owner_user_id, id);
