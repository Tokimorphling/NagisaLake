-- Index for the job list's own sort order.
--
-- The list is ordered by created_at DESC. Neither existing index serves it:
-- idx_jobs_org_updated sorts by updated_at, and
-- idx_jobs_organization_actor_user_created has actor_user_id between the
-- organization and the timestamp, so a query that does not filter on
-- actor_user_id cannot use its sort. A LIMIT 50 over 100k rows measured as a
-- parallel sequential scan plus top-N sort (37 ms, 8423 shared buffers).
--
-- `id` is part of the key so keyset pagination has a stable tiebreaker: jobs
-- created in the same millisecond would otherwise be skipped or repeated when
-- paging on created_at alone.
CREATE INDEX IF NOT EXISTS idx_jobs_org_created_id
    ON jobs (organization_id, created_at DESC, id DESC);
