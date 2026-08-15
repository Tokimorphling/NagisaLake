-- Repair installations where the batch table predates the tenant-scoped key.
--
-- 0012 creates this key for fresh databases, but CREATE TABLE IF NOT EXISTS
-- intentionally leaves an already-existing table untouched.  Such a table
-- cannot satisfy the composite foreign key used by batch idempotency rows.
-- Keep this as a forward migration because 0012 is already published.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'job_batches'::regclass
          AND contype = 'u'
          AND pg_get_constraintdef(oid) = 'UNIQUE (organization_id, id)'
    )
    AND NOT EXISTS (
        SELECT 1
        FROM pg_class index_class
        JOIN pg_index index_metadata ON index_metadata.indexrelid = index_class.oid
        WHERE index_metadata.indrelid = 'job_batches'::regclass
          AND index_metadata.indisunique
          AND index_metadata.indpred IS NULL
          AND index_metadata.indexprs IS NULL
          AND pg_get_indexdef(index_class.oid) LIKE '%(organization_id, id)%'
    ) THEN
        CREATE UNIQUE INDEX idx_job_batches_organization_id_id
            ON job_batches (organization_id, id);
    END IF;
END
$$;
