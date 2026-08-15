-- Gives reserved-but-never-uploaded artifacts a deadline.
--
-- Creating an upload reserves the full storage quota before the presigned URL
-- is handed out. A client that never PUTs and never completes leaves the
-- artifact in 'pending_upload' forever, holding quota against zero bytes in
-- object storage. Without an expiry there is nothing to reclaim against.
--
-- Only pending uploads expire. A 'ready' artifact is real data and keeps its
-- quota until the artifact itself is removed.
ALTER TABLE artifacts ADD COLUMN IF NOT EXISTS expires_at BIGINT;

-- Existing rows predate the column. Pending ones are already abandoned by
-- definition, so mark them immediately collectable; ready ones never expire.
UPDATE artifacts
SET expires_at = created_at
WHERE expires_at IS NULL AND state = 'pending_upload';

-- Partial index: the reaper only ever scans pending uploads with a deadline.
CREATE INDEX IF NOT EXISTS idx_artifacts_pending_expiry
    ON artifacts (expires_at)
    WHERE state = 'pending_upload' AND expires_at IS NOT NULL;
