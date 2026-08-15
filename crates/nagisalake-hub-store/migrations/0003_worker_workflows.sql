CREATE TABLE IF NOT EXISTS worker_workflows (
    organization_id TEXT NOT NULL,
    worker_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    version TEXT NOT NULL,
    last_seen_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, worker_id, workflow_id, version),
    FOREIGN KEY (organization_id, worker_id)
        REFERENCES workers(organization_id, id) ON DELETE CASCADE,
    FOREIGN KEY (organization_id, workflow_id, version)
        REFERENCES workflow_versions(organization_id, workflow_id, version) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_worker_workflows_catalog
    ON worker_workflows(organization_id, workflow_id, version);
