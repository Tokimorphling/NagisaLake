-- Federated sign-in.
--
-- Delegating identity to Google/GitHub removes three things this deployment does
-- not implement: email verification, password reset and password change. A user
-- who cannot recover their account has no path back in, so until those exist,
-- OAuth is the only safe way to open registration.

-- Password login stays available, so an account may now have no password at all.
-- NOT NULL would have forced a synthetic hash for every federated user, which
-- then looks like a credential that can be used.
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;

CREATE TABLE IF NOT EXISTS user_identities (
    provider TEXT NOT NULL,
    -- The provider's immutable identifier for the account. Deliberately not the
    -- email: emails get reassigned, and keying on one would hand the account to
    -- whoever receives the address next.
    subject TEXT NOT NULL,
    user_id TEXT NOT NULL,
    -- Address as seen at link time, kept for display and support. Authorization
    -- never reads it.
    email TEXT,
    -- Whether the provider asserted the address was verified. Linking to an
    -- existing local account requires this: GitHub lets anyone attach an
    -- arbitrary address, so trusting an unverified one is an account takeover.
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    created_at BIGINT NOT NULL,
    last_login_at BIGINT,
    PRIMARY KEY (provider, subject),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

-- One provider account per user, so a second Google login cannot silently
-- attach itself to an account that already has one.
CREATE UNIQUE INDEX IF NOT EXISTS idx_user_identities_user_provider
    ON user_identities (user_id, provider);

CREATE INDEX IF NOT EXISTS idx_user_identities_user
    ON user_identities (user_id);

-- Short-lived authorization requests. Held server-side rather than in a cookie
-- so a replayed callback can be rejected exactly once, and so the PKCE verifier
-- never reaches the browser.
CREATE TABLE IF NOT EXISTS oauth_authorizations (
    state TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    pkce_verifier TEXT NOT NULL,
    -- Where to send the browser afterwards. Validated as a same-site relative
    -- path before it is stored, so the callback cannot be turned into an open
    -- redirect.
    redirect_path TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    consumed_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_oauth_authorizations_expiry
    ON oauth_authorizations (expires_at)
    WHERE consumed_at IS NULL;
