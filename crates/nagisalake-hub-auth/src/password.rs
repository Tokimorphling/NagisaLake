//! Human password handling: Argon2id hashing with a bounded hashing pool.

use crate::error::AuthError;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use std::{num::NonZeroUsize, sync::LazyLock};
use tokio::sync::Semaphore;
use uuid::Uuid;

const MIN_PASSWORD_BYTES: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1_024;

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
pub(crate) const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,\
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
