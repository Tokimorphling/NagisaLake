//! Federated sign-in with Google, GitHub, Linux.do, or any OIDC-style provider.
//!
//! Delegating identity removes three capabilities this deployment does not have:
//! email verification, password reset and password change. Without them a user
//! who forgets their password has no way back in, so federated login is the only
//! safe way to open registration.
//!
//! The flow is authorization code with PKCE. Provider tokens are used once to
//! read the identity and then dropped: nothing here needs ongoing access to the
//! user's Google or GitHub account, and not storing the token removes it as
//! something that can leak.

mod config;
mod exchange;
mod pkce;

/// Only test builds construct provider configs outside this module.
#[cfg(test)]
pub(crate) use config::ProviderConfig;
pub use config::{AUTHORIZATION_TTL_SECONDS, OauthConfig, Provider, ProviderKind};
pub use exchange::exchange_and_fetch_identity;
pub use pkce::{authorize_url, generate_pkce, generate_state, sanitize_redirect_path};

#[cfg(test)]
mod tests;
