//! OAuth configuration, provider resolution, and identity types.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How long an authorization request stays valid.
pub const AUTHORIZATION_TTL_SECONDS: i64 = 600;

#[derive(Debug, Clone, Deserialize)]
pub struct OauthConfig {
    /// Absolute base URL this Hub is reached at, used to build the redirect URI.
    /// Must match what is registered with the provider exactly.
    pub public_url: String,
    #[serde(default)]
    pub providers:  BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// `google`, `github`, `linuxdo`, or `oidc` for anything else.
    pub kind:                  ProviderKind,
    pub client_id:             String,
    /// Read from this environment variable rather than the config file, so the
    /// secret is not committed alongside the rest of the settings.
    pub client_secret_env:     String,
    /// Required for `oidc`; ignored for the built-in providers.
    #[serde(default)]
    pub authorize_url:         Option<String>,
    #[serde(default)]
    pub token_url:             Option<String>,
    #[serde(default)]
    pub userinfo_url:          Option<String>,
    #[serde(default)]
    pub scopes:                Option<Vec<String>>,
    /// Restricts sign-in to these email domains. Empty means any domain, which
    /// on a public deployment means anyone with a Google account.
    #[serde(default)]
    pub allowed_email_domains: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Google,
    Github,
    Linuxdo,
    Oidc,
}

impl ProviderKind {
    pub(super) const fn authorize_url(self) -> &'static str {
        match self {
            Self::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            Self::Github => "https://github.com/login/oauth/authorize",
            Self::Linuxdo => "https://connect.linux.do/oauth2/authorize",
            Self::Oidc => "",
        }
    }

    pub(super) const fn token_url(self) -> &'static str {
        match self {
            Self::Google => "https://oauth2.googleapis.com/token",
            Self::Github => "https://github.com/login/oauth/access_token",
            Self::Linuxdo => "https://connect.linux.do/oauth2/token",
            Self::Oidc => "",
        }
    }

    pub(super) const fn userinfo_url(self) -> &'static str {
        match self {
            Self::Google => "https://openidconnect.googleapis.com/v1/userinfo",
            Self::Github => "https://api.github.com/user",
            Self::Linuxdo => "https://connect.linux.do/api/user",
            Self::Oidc => "",
        }
    }

    pub(super) fn default_scopes(self) -> Vec<String> {
        match self {
            Self::Google => vec!["openid".into(), "email".into(), "profile".into()],
            // `user:email` is required to reach /user/emails, which is the only
            // way to learn whether an address is verified.
            Self::Github => vec!["read:user".into(), "user:email".into()],
            Self::Linuxdo => vec!["user".into()],
            Self::Oidc => vec!["openid".into(), "email".into()],
        }
    }
}

/// A provider resolved from configuration, with its secret loaded.
#[derive(Debug, Clone)]
pub struct Provider {
    pub name:                  String,
    pub kind:                  ProviderKind,
    pub client_id:             String,
    pub client_secret:         String,
    pub authorize_url:         String,
    pub token_url:             String,
    pub userinfo_url:          String,
    pub scopes:                Vec<String>,
    pub allowed_email_domains: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OauthError {
    #[error("invalid OAuth configuration: {0}")]
    InvalidConfig(String),
    #[error("unknown OAuth provider {0}")]
    UnknownProvider(String),
    #[error("OAuth provider request failed: {0}")]
    Provider(String),
    #[error("the provider did not return a usable identity: {0}")]
    Identity(String),
}

/// The identity a provider asserted.
#[derive(Debug, Clone)]
pub struct FederatedIdentity {
    /// The provider's immutable identifier. Never the email.
    pub subject:        String,
    pub email:          String,
    /// Whether the provider stated the address is verified. Linking to an
    /// existing local account depends on this.
    pub email_verified: bool,
    pub display_name:   Option<String>,
}

impl OauthConfig {
    /// Resolves every configured provider and loads its secret.
    ///
    /// Fails on a missing secret rather than skipping the provider: a sign-in
    /// button that 500s is worse than a Hub that refuses to start.
    pub fn resolve(&self) -> Result<BTreeMap<String, Provider>, OauthError> {
        if self.public_url.trim().is_empty() {
            return Err(OauthError::InvalidConfig(
                "oauth.public_url is required to build the redirect URI".into(),
            ));
        }
        if !self.public_url.starts_with("http://") && !self.public_url.starts_with("https://") {
            return Err(OauthError::InvalidConfig(
                "oauth.public_url must include the scheme".into(),
            ));
        }
        let mut resolved = BTreeMap::new();
        for (name, config) in &self.providers {
            let client_secret = std::env::var(&config.client_secret_env).map_err(|_| {
                OauthError::InvalidConfig(format!(
                    "provider {name}: environment variable {} is not set",
                    config.client_secret_env
                ))
            })?;
            if client_secret.trim().is_empty() {
                return Err(OauthError::InvalidConfig(format!(
                    "provider {name}: {} is empty",
                    config.client_secret_env
                )));
            }
            if config.client_id.trim().is_empty() {
                return Err(OauthError::InvalidConfig(format!(
                    "provider {name}: client_id is empty"
                )));
            }
            let pick = |explicit: Option<&String>, builtin: &str, field: &str| {
                explicit
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .or_else(|| (!builtin.is_empty()).then(|| builtin.to_owned()))
                    .ok_or_else(|| {
                        OauthError::InvalidConfig(format!("provider {name}: {field} is required"))
                    })
            };
            resolved.insert(name.clone(), Provider {
                name: name.clone(),
                kind: config.kind,
                client_id: config.client_id.trim().to_owned(),
                client_secret,
                authorize_url: pick(
                    config.authorize_url.as_ref(),
                    config.kind.authorize_url(),
                    "authorize_url",
                )?,
                token_url: pick(
                    config.token_url.as_ref(),
                    config.kind.token_url(),
                    "token_url",
                )?,
                userinfo_url: pick(
                    config.userinfo_url.as_ref(),
                    config.kind.userinfo_url(),
                    "userinfo_url",
                )?,
                scopes: config
                    .scopes
                    .clone()
                    .filter(|scopes| !scopes.is_empty())
                    .unwrap_or_else(|| config.kind.default_scopes()),
                allowed_email_domains: config
                    .allowed_email_domains
                    .iter()
                    .map(|domain| domain.trim().to_ascii_lowercase())
                    .filter(|domain| !domain.is_empty())
                    .collect(),
            });
        }
        Ok(resolved)
    }

    pub fn redirect_uri(&self, provider: &str) -> String {
        format!(
            "{}/api/v1/auth/oauth/{provider}/callback",
            self.public_url.trim_end_matches('/')
        )
    }
}

impl Provider {
    /// Whether this address is allowed to sign in.
    pub fn allows_email(&self, email: &str) -> bool {
        if self.allowed_email_domains.is_empty() {
            return true;
        }
        let domain = email
            .rsplit_once('@')
            .map(|(_local, domain)| domain.to_ascii_lowercase())
            .unwrap_or_default();
        self.allowed_email_domains.contains(&domain)
    }
}
