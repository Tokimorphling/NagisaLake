-- Business history is independent of OpenCode session storage. Session ids,
-- tool payloads, credentials and reasoning are intentionally not persisted.
CREATE TABLE agent_executions (
    organization_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    skill TEXT NOT NULL,
    skill_version TEXT NOT NULL,
    input TEXT NOT NULL,
    options_json TEXT NOT NULL,
    output TEXT,
    state TEXT NOT NULL CHECK (state IN ('running', 'completed', 'failed', 'cancelled')),
    error_code TEXT,
    started_at BIGINT NOT NULL,
    completed_at BIGINT,
    deadline_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, id)
);
CREATE INDEX idx_agent_executions_owner_created
    ON agent_executions (organization_id, owner_id, started_at DESC, id DESC);
