//! Authentication primitives and organization authorization policy for the Hub.
//!
//! This crate deliberately has no HTTP or database dependencies. Browser
//! sessions, API keys, and worker credentials share secret-handling helpers but
//! remain distinct principal kinds throughout authorization and audit records.

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt::{self, Write as _},
    num::NonZeroUsize,
    str::FromStr,
    sync::LazyLock,
};
use thiserror::Error;
use tokio::sync::Semaphore;
use uuid::Uuid;

const MIN_PASSWORD_BYTES: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1_024;

/// Organization role ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Member,
    Operator,
    Admin,
    Owner,
}

impl Role {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Member => 1,
            Self::Operator => 2,
            Self::Admin => 3,
            Self::Owner => 4,
        }
    }

    pub const fn allows(self, permission: Permission) -> bool {
        match permission {
            Permission::WorkflowsRead
            | Permission::JobsReadOrganization
            | Permission::QuotaRead => self.rank() >= Self::Viewer.rank(),
            Permission::JobsWrite
            | Permission::JobsCancelOwn
            | Permission::ArtifactsRead
            | Permission::ArtifactsWrite
            | Permission::ApiKeysManageOwn
            | Permission::DevicesRead
            | Permission::DevicesUse
            | Permission::DevicesRegisterOwn
            | Permission::DevicesShareOwn => self.rank() >= Self::Member.rank(),
            Permission::JobsCancelAny
            | Permission::WorkersManage
            | Permission::WorkflowsPublish => self.rank() >= Self::Operator.rank(),
            Permission::MembersManage
            | Permission::ApiKeysManage
            | Permission::QuotaManage
            | Permission::AuditRead => self.rank() >= Self::Admin.rank(),
            Permission::OrganizationDelete => matches!(self, Self::Owner),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Viewer => "viewer",
            Self::Member => "member",
            Self::Operator => "operator",
            Self::Admin => "admin",
            Self::Owner => "owner",
        })
    }
}

impl FromStr for Role {
    type Err = AuthError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "viewer" => Ok(Self::Viewer),
            "member" => Ok(Self::Member),
            "operator" => Ok(Self::Operator),
            "admin" => Ok(Self::Admin),
            "owner" => Ok(Self::Owner),
            _ => Err(AuthError::InvalidRole(value.into())),
        }
    }
}

/// Stable service permissions used by browser roles and API-key scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    WorkflowsRead,
    WorkflowsPublish,
    JobsReadOrganization,
    JobsWrite,
    JobsCancelOwn,
    JobsCancelAny,
    ArtifactsRead,
    ArtifactsWrite,
    WorkersManage,
    MembersManage,
    ApiKeysManageOwn,
    ApiKeysManage,
    QuotaRead,
    QuotaManage,
    AuditRead,
    OrganizationDelete,
    DevicesRead,
    DevicesUse,
    DevicesRegisterOwn,
    DevicesShareOwn,
}

impl Permission {
    pub const fn scope(self) -> &'static str {
        match self {
            Self::WorkflowsRead => "workflows:read",
            Self::WorkflowsPublish => "workflows:write",
            Self::JobsReadOrganization => "jobs:read",
            Self::JobsWrite => "jobs:write",
            Self::JobsCancelOwn | Self::JobsCancelAny => "jobs:cancel",
            Self::ArtifactsRead => "artifacts:read",
            Self::ArtifactsWrite => "artifacts:write",
            Self::WorkersManage => "workers:manage",
            Self::MembersManage => "members:manage",
            Self::ApiKeysManageOwn | Self::ApiKeysManage => "api_keys:manage",
            Self::QuotaRead => "quota:read",
            Self::QuotaManage => "quota:manage",
            Self::AuditRead => "audit:read",
            Self::OrganizationDelete => "organizations:delete",
            Self::DevicesRead => "devices:read",
            Self::DevicesUse => "devices:use",
            Self::DevicesRegisterOwn => "devices:register",
            Self::DevicesShareOwn => "devices:share",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    BrowserSession,
    ApiKey,
    WorkerCredential,
    LegacyToken,
}

/// Authenticated actor passed from transport authentication into services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub kind:            PrincipalKind,
    pub actor_id:        String,
    pub user_id:         Option<String>,
    pub organization_id: String,
    pub role:            Role,
    pub scopes:          BTreeSet<String>,
}

impl Principal {
    /// Both organization role and API-key scope must authorize the operation.
    pub fn allows(&self, permission: Permission) -> bool {
        if !self.role.allows(permission) {
            return false;
        }
        self.kind != PrincipalKind::ApiKey || self.scopes.contains(permission.scope())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedSecret {
    pub plaintext:      String,
    pub display_prefix: String,
    pub hash:           String,
}

/// Generates a high-entropy opaque token. The full value is returned once.
pub fn generate_secret(prefix: &str) -> GeneratedSecret {
    let random = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let plaintext = format!("{prefix}_{random}");
    let display_prefix = plaintext.chars().take(prefix.len() + 9).collect();
    let hash = hash_secret(&plaintext);
    GeneratedSecret {
        plaintext,
        display_prefix,
        hash,
    }
}

/// SHA-256 is appropriate for uniformly random 244-bit bearer secrets.
pub fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(hash, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hash
}

pub fn verify_secret(secret: &str, expected_hash: &str) -> bool {
    constant_time_eq(hash_secret(secret).as_bytes(), expected_hash.as_bytes())
}

/// Hashes a human password with Argon2id and a fresh random salt.
///
/// Argon2 is deliberately CPU-heavy. Prefer [`hash_password_async`] from async
/// code so the work does not occupy a Tokio worker thread.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    validate_password(password)?;
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|error| AuthError::PasswordHash(error.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| AuthError::PasswordHash(error.to_string()))
}

pub fn verify_password(password: &str, encoded: &str) -> bool {
    let Ok(hash) = PasswordHash::new(encoded) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .is_ok()
}

/// An Argon2id hash of an unguessable value, used to spend the same CPU on a
/// login for an address that is not registered.
///
/// Without this, a missing account skips Argon2 entirely and answers about an
/// order of magnitude faster than a wrong password for a real account, which
/// turns the login endpoint into an account enumeration oracle.
/// It must stay parseable: a malformed value makes `verify_password` bail out
/// before Argon2 runs, silently restoring the timing difference. A test asserts
/// this, so regenerate with the same Argon2 defaults if it ever changes.
const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,\
                                   p=1$bI5xn1huTl6RzkmF2YNKTQ$SA7FM3nFp/\
                                   if4oq8YRm4bfKksCYL1k6dXnTxzhB//74";

/// Caps how many password hashes run at once.
///
/// `spawn_blocking` alone keeps Argon2 off the async worker threads, but it does
/// not bound CPU: enough concurrent logins still saturate every core and slow
/// down unrelated requests. Half the cores leaves room for the rest of the
/// server, and the queue this creates also throttles credential stuffing.
static PASSWORD_HASH_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| {
    let parallelism = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(2);
    Semaphore::new((parallelism / 2).max(2))
});

/// Runs `work` on a blocking thread, bounded by [`PASSWORD_HASH_SLOTS`].
async fn with_password_slot<T, F>(work: F) -> Result<T, AuthError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let _permit = PASSWORD_HASH_SLOTS
        .acquire()
        .await
        .map_err(|_| AuthError::PasswordHash("password hashing pool is closed".into()))?;
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AuthError::PasswordHash(error.to_string()))
}

/// Verifies a password without blocking an async worker thread.
///
/// `stored` is `None` when the account does not exist. The dummy hash is still
/// verified in that case so the response time does not reveal which it was, and
/// the result is always `false`.
pub async fn verify_password_async(password: String, stored: Option<String>) -> bool {
    with_password_slot(move || match stored {
        Some(encoded) => verify_password(&password, &encoded),
        None => {
            // Result intentionally discarded: this exists only to spend the same
            // CPU a real verification would.
            let _ = verify_password(&password, DUMMY_PASSWORD_HASH);
            false
        }
    })
    .await
    .unwrap_or(false)
}

/// Hashes a password without blocking an async worker thread.
pub async fn hash_password_async(password: String) -> Result<String, AuthError> {
    with_password_slot(move || hash_password(&password)).await?
}

pub fn validate_password(password: &str) -> Result<(), AuthError> {
    let bytes = password.len();
    if bytes < MIN_PASSWORD_BYTES {
        return Err(AuthError::WeakPassword(format!(
            "password must be at least {MIN_PASSWORD_BYTES} bytes"
        )));
    }
    if bytes > MAX_PASSWORD_BYTES {
        return Err(AuthError::WeakPassword(format!(
            "password must be at most {MAX_PASSWORD_BYTES} bytes"
        )));
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut different = 0_u8;
    for (left, right) in left.iter().zip(right) {
        different |= left ^ right;
    }
    different == 0
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid organization role: {0}")]
    InvalidRole(String),
    #[error("weak password: {0}")]
    WeakPassword(String),
    #[error("password hashing failed: {0}")]
    PasswordHash(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dummy hash only equalises login timing if it actually parses. A
    /// malformed value makes `verify_password` return before Argon2 runs, which
    /// silently reopens the account enumeration oracle.
    #[test]
    fn dummy_password_hash_is_a_usable_argon2_hash() {
        let parsed = PasswordHash::new(DUMMY_PASSWORD_HASH)
            .expect("dummy hash must parse, or no Argon2 work happens");
        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        // Wrong password against a well-formed hash: rejected, but only after a
        // full verification.
        assert!(!verify_password("anything", DUMMY_PASSWORD_HASH));
    }

    /// A missing account must consume comparable CPU to a wrong password.
    #[tokio::test]
    async fn unknown_accounts_still_pay_for_a_verification() {
        let real = hash_password("correct horse battery staple").unwrap();

        let wrong_password = std::time::Instant::now();
        assert!(!verify_password_async("wrong".into(), Some(real)).await);
        let wrong_password = wrong_password.elapsed();

        let unknown_account = std::time::Instant::now();
        assert!(!verify_password_async("wrong".into(), None).await);
        let unknown_account = unknown_account.elapsed();

        // Same order of magnitude. A skipped Argon2 would be 10x+ faster, so
        // this catches a regression without being flaky on a loaded machine.
        let ratio = wrong_password.as_secs_f64() / unknown_account.as_secs_f64().max(1e-9);
        assert!(
            (0.2..=5.0).contains(&ratio),
            "timing should be comparable, got {ratio:.2}x (known {wrong_password:?} vs unknown \
             {unknown_account:?})"
        );
    }

    #[test]
    fn passwords_use_salted_argon2_hashes() {
        let first = hash_password("correct horse battery staple").unwrap();
        let second = hash_password("correct horse battery staple").unwrap();
        assert_ne!(first, second);
        assert!(verify_password("correct horse battery staple", &first));
        assert!(!verify_password("incorrect horse battery staple", &first));
    }

    #[test]
    fn secrets_are_prefixed_hashed_and_verifiable() {
        let key = generate_secret("nsk");
        assert!(key.plaintext.starts_with("nsk_"));
        assert!(!key.hash.contains(&key.plaintext));
        assert!(verify_secret(&key.plaintext, &key.hash));
        assert!(!verify_secret("nsk_wrong", &key.hash));
    }

    #[test]
    fn api_key_scope_cannot_bypass_role() {
        let principal = Principal {
            kind:            PrincipalKind::ApiKey,
            actor_id:        "key".into(),
            user_id:         Some("user".into()),
            organization_id: "org".into(),
            role:            Role::Member,
            scopes:          BTreeSet::from(["members:manage".into(), "jobs:write".into()]),
        };
        assert!(principal.allows(Permission::JobsWrite));
        assert!(!principal.allows(Permission::MembersManage));
    }

    #[test]
    fn role_matrix_matches_product_policy() {
        assert!(Role::Viewer.allows(Permission::WorkflowsRead));
        assert!(!Role::Viewer.allows(Permission::JobsWrite));
        assert!(Role::Operator.allows(Permission::WorkersManage));
        assert!(!Role::Operator.allows(Permission::MembersManage));
        assert!(Role::Admin.allows(Permission::AuditRead));
        assert!(!Role::Admin.allows(Permission::OrganizationDelete));
        assert!(Role::Owner.allows(Permission::OrganizationDelete));
    }
}
