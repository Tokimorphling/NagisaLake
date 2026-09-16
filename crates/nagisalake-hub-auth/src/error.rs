//! Hub authentication errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid organization role: {0}")]
    InvalidRole(String),
    #[error("weak password: {0}")]
    WeakPassword(String),
    #[error("password hashing failed: {0}")]
    PasswordHash(String),
}
