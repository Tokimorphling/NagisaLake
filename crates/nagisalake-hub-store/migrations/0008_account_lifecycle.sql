-- Lockout state is kept with the account so a restart does not erase the
-- protection. The in-memory IP/account buckets still absorb the request burst.
ALTER TABLE users
    ADD COLUMN IF NOT EXISTS failed_login_attempts BIGINT NOT NULL DEFAULT 0;
ALTER TABLE users
    ADD COLUMN IF NOT EXISTS locked_until BIGINT;
