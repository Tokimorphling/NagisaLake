-- One-time organization invitations. Delivery is intentionally out of band:
-- an admin copies the noi_ secret to the intended OAuth user. Only its hash is
-- stored and acceptance creates a normal RBAC membership.
CREATE TABLE IF NOT EXISTS organization_invites (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    inviter_user_id TEXT NOT NULL,
    code_prefix TEXT NOT NULL,
    code_hash TEXT NOT NULL UNIQUE,
    role TEXT NOT NULL CHECK (role IN ('viewer', 'member', 'operator', 'admin')),
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    accepted_at BIGINT,
    accepted_by_user_id TEXT,
    revoked_at BIGINT,
    FOREIGN KEY (organization_id, inviter_user_id)
        REFERENCES memberships(organization_id, user_id) ON DELETE CASCADE,
    FOREIGN KEY (accepted_by_user_id) REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_organization_invites_org_created
    ON organization_invites (organization_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_organization_invites_active
    ON organization_invites (code_hash, expires_at)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;
