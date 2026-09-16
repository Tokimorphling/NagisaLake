//! Authentication primitives and organization authorization policy for the Hub.
//!
//! This crate deliberately has no HTTP or database dependencies. Browser
//! sessions, API keys, and worker credentials share secret-handling helpers but
//! remain distinct principal kinds throughout authorization and audit records.
//!
//! ## Key Components
//!
//! - [`Role`] / [`Permission`] / [`Principal`]: organization authorization.
//! - [`generate_secret`] / [`hash_secret`]: opaque bearer token handling.
//! - [`hash_password_async`]: Argon2id with a bounded hashing pool.

mod error;
mod password;
mod principal;
mod secret;

pub use error::AuthError;
pub use password::{
    hash_password, hash_password_async, validate_password, verify_password, verify_password_async,
};
pub use principal::{Permission, Principal, PrincipalKind, Role};
pub use secret::{GeneratedSecret, generate_secret, hash_secret, verify_secret};

#[cfg(test)]
mod tests;
