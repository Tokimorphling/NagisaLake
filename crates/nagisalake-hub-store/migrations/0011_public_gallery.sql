-- A gallery publication is a capability boundary over an otherwise private
-- artifact.  The object key and bucket URL deliberately remain in the
-- artifacts table; signed-in readers receive a Hub stream or short-lived media
-- ticket only after this row has been found.
CREATE TABLE IF NOT EXISTS gallery_items (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    display_name TEXT NOT NULL,
    parameters_json TEXT NOT NULL,
    published_at BIGINT NOT NULL,
    UNIQUE (organization_id, artifact_id),
    FOREIGN KEY (organization_id, artifact_id)
        REFERENCES artifacts(organization_id, id) ON DELETE CASCADE,
    FOREIGN KEY (organization_id, job_id)
        REFERENCES jobs(organization_id, id) ON DELETE CASCADE,
    FOREIGN KEY (organization_id, owner_user_id)
        REFERENCES memberships(organization_id, user_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_gallery_items_published_id
    ON gallery_items (published_at DESC, id DESC);
