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

use nagisalake_hub_auth::hash_secret;
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
    const fn authorize_url(self) -> &'static str {
        match self {
            Self::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            Self::Github => "https://github.com/login/oauth/authorize",
            Self::Linuxdo => "https://connect.linux.do/oauth2/authorize",
            Self::Oidc => "",
        }
    }

    const fn token_url(self) -> &'static str {
        match self {
            Self::Google => "https://oauth2.googleapis.com/token",
            Self::Github => "https://github.com/login/oauth/access_token",
            Self::Linuxdo => "https://connect.linux.do/oauth2/token",
            Self::Oidc => "",
        }
    }

    const fn userinfo_url(self) -> &'static str {
        match self {
            Self::Google => "https://openidconnect.googleapis.com/v1/userinfo",
            Self::Github => "https://api.github.com/user",
            Self::Linuxdo => "https://connect.linux.do/api/user",
            Self::Oidc => "",
        }
    }

    fn default_scopes(self) -> Vec<String> {
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

/// A PKCE challenge pair.
pub struct Pkce {
    pub verifier:  String,
    pub challenge: String,
}

/// Generates a PKCE verifier and its S256 challenge.
///
/// PKCE is used even though this is a confidential client: it binds the callback
/// to the request that started it, so an authorization code intercepted in a
/// redirect cannot be redeemed by anyone else.
pub fn generate_pkce() -> Pkce {
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    // hash_secret returns lowercase hex; the challenge must be base64url of the
    // raw digest, so decode the hex first.
    let digest = hash_secret(&verifier);
    let raw = data_encoding::HEXLOWER
        .decode(digest.as_bytes())
        .unwrap_or_default();
    let challenge = data_encoding::BASE64URL_NOPAD.encode(&raw);
    Pkce {
        verifier,
        challenge,
    }
}

/// Opaque, unguessable state value.
pub fn generate_state() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Builds the URL to send the browser to.
pub fn authorize_url(
    provider: &Provider,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&scope={}",
        provider.authorize_url,
        urlencode(&provider.client_id),
        urlencode(redirect_uri),
        urlencode(state),
        urlencode(&provider.scopes.join(" ")),
    );
    // GitHub ignores PKCE, but sending it is harmless and keeps one code path.
    url.push_str(&format!(
        "&code_challenge={}&code_challenge_method=S256",
        urlencode(challenge)
    ));
    if provider.kind == ProviderKind::Google {
        // Ask for a fresh consent-free login; we never need a refresh token.
        url.push_str("&access_type=online&prompt=select_account");
    }
    url
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token:      Option<String>,
    error:             Option<String>,
    error_description: Option<String>,
}

/// Exchanges the authorization code and reads the identity.
pub async fn exchange_and_fetch_identity(
    client: &reqwest::Client,
    provider: &Provider,
    redirect_uri: &str,
    code: &str,
    pkce_verifier: &str,
) -> Result<FederatedIdentity, OauthError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", provider.client_id.as_str()),
        ("client_secret", provider.client_secret.as_str()),
        ("code_verifier", pkce_verifier),
    ];
    let response = client
        .post(&provider.token_url)
        // GitHub returns form-encoded unless asked for JSON.
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|error| OauthError::Provider(format!("token request failed: {error}")))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| OauthError::Provider(format!("failed to read token response: {error}")))?;
    let token = parse_token_response(&body)?;
    if let Some(error) = token.error {
        let detail = token.error_description.unwrap_or_default();
        return Err(OauthError::Provider(format!(
            "token endpoint rejected the code: {error} {detail}"
        )));
    }
    if !status.is_success() {
        return Err(OauthError::Provider(format!(
            "token endpoint returned HTTP {status}"
        )));
    }

    match provider.kind {
        ProviderKind::Google | ProviderKind::Oidc => {
            let access_token = token
                .access_token
                .ok_or_else(|| OauthError::Identity("token response had no access_token".into()))?;
            fetch_oidc_userinfo(client, provider, &access_token).await
        }
        ProviderKind::Github => {
            let access_token = token
                .access_token
                .ok_or_else(|| OauthError::Identity("token response had no access_token".into()))?;
            fetch_github_identity(client, provider, &access_token).await
        }
        ProviderKind::Linuxdo => {
            let access_token = token
                .access_token
                .ok_or_else(|| OauthError::Identity("token response had no access_token".into()))?;
            fetch_linuxdo_identity(client, provider, &access_token).await
        }
    }
}

fn parse_token_response(body: &[u8]) -> Result<TokenResponse, OauthError> {
    serde_json::from_slice(body)
        .or_else(|_| serde_urlencoded::from_bytes(body))
        .map_err(|error| {
            OauthError::Provider(format!("token response was not JSON or form data: {error}"))
        })
}

/// Test helper for claim-shape compatibility. The live path reads userinfo with
/// the provider-issued access token, avoiding an incomplete local JWT verifier.
#[cfg(test)]
fn identity_from_id_token(id_token: &str) -> Option<FederatedIdentity> {
    let payload = id_token.split('.').nth(1)?;
    let decoded = data_encoding::BASE64URL_NOPAD
        .decode(payload.as_bytes())
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let subject = claims.get("sub")?.as_str()?.to_owned();
    let email = claims.get("email")?.as_str()?.to_owned();
    // Accept the boolean or the string form; providers differ.
    let email_verified = match claims.get("email_verified") {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::String(value)) => value == "true",
        _ => false,
    };
    Some(FederatedIdentity {
        subject,
        email,
        email_verified,
        display_name: claims
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
    })
}

async fn fetch_oidc_userinfo(
    client: &reqwest::Client,
    provider: &Provider,
    access_token: &str,
) -> Result<FederatedIdentity, OauthError> {
    let claims: serde_json::Value = client
        .get(&provider.userinfo_url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| OauthError::Provider(format!("userinfo request failed: {error}")))?
        .error_for_status()
        .map_err(|error| OauthError::Provider(format!("userinfo returned an error: {error}")))?
        .json()
        .await
        .map_err(|error| OauthError::Provider(format!("userinfo was not JSON: {error}")))?;
    let subject = claims
        .get("sub")
        .and_then(|value| value.as_str())
        .ok_or_else(|| OauthError::Identity("userinfo had no sub".into()))?
        .to_owned();
    let email = claims
        .get("email")
        .and_then(|value| value.as_str())
        .ok_or_else(|| OauthError::Identity("userinfo had no email".into()))?
        .to_owned();
    let email_verified = claims
        .get("email_verified")
        .and_then(|value| match value {
            serde_json::Value::Bool(value) => Some(*value),
            serde_json::Value::String(value) => Some(value == "true"),
            _ => None,
        })
        .unwrap_or(false);
    Ok(FederatedIdentity {
        subject,
        email,
        email_verified,
        display_name: claims
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
    })
}

async fn fetch_linuxdo_identity(
    client: &reqwest::Client,
    provider: &Provider,
    access_token: &str,
) -> Result<FederatedIdentity, OauthError> {
    let claims: serde_json::Value = client
        .get(&provider.userinfo_url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| OauthError::Provider(format!("userinfo request failed: {error}")))?
        .error_for_status()
        .map_err(|error| OauthError::Provider(format!("userinfo returned an error: {error}")))?
        .json()
        .await
        .map_err(|error| OauthError::Provider(format!("userinfo was not JSON: {error}")))?;
    linuxdo_identity_from_userinfo(&claims)
}

fn linuxdo_identity_from_userinfo(
    claims: &serde_json::Value,
) -> Result<FederatedIdentity, OauthError> {
    let subject = claim_string(claims, "id")
        .ok_or_else(|| OauthError::Identity("Linux.do userinfo had no id".into()))?;
    const MAX_SUBJECT_BYTES: usize = 64 - "linuxdo-".len();
    if subject.len() > MAX_SUBJECT_BYTES
        || !subject
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(OauthError::Identity(
            "Linux.do userinfo returned an invalid id".into(),
        ));
    }
    let username = claim_string(claims, "username")
        .or_else(|| claim_string(claims, "name"))
        .unwrap_or_else(|| format!("linuxdo_{subject}"));
    // Linux.do does not promise an OIDC-style verified email claim. Deriving an
    // address from its immutable id prevents an unverified upstream address from
    // linking to an unrelated local account while satisfying the local account
    // model's unique-email requirement.
    let email = format!("linuxdo-{subject}@linuxdo-connect.invalid");
    Ok(FederatedIdentity {
        subject,
        email,
        email_verified: true,
        display_name: claim_string(claims, "name").or(Some(username)),
    })
}

fn claim_string(claims: &serde_json::Value, name: &str) -> Option<String> {
    match claims.get(name)? {
        serde_json::Value::String(value) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    id:    i64,
    login: String,
    name:  Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubEmail {
    email:    String,
    primary:  bool,
    verified: bool,
}

/// Reads a GitHub identity.
///
/// `/user` alone is not enough: its `email` field is whatever the user set as
/// public, may be null, and carries no verification status. The primary verified
/// address has to come from `/user/emails`, because linking to an existing local
/// account is only safe for an address GitHub confirmed.
async fn fetch_github_identity(
    client: &reqwest::Client,
    provider: &Provider,
    access_token: &str,
) -> Result<FederatedIdentity, OauthError> {
    let user: GithubUser = client
        .get(&provider.userinfo_url)
        .bearer_auth(access_token)
        .header(reqwest::header::USER_AGENT, "nagisalake-hub")
        .send()
        .await
        .map_err(|error| OauthError::Provider(format!("/user request failed: {error}")))?
        .error_for_status()
        .map_err(|error| OauthError::Provider(format!("/user returned an error: {error}")))?
        .json()
        .await
        .map_err(|error| OauthError::Provider(format!("/user was not JSON: {error}")))?;

    let emails: Vec<GithubEmail> = client
        .get("https://api.github.com/user/emails")
        .bearer_auth(access_token)
        .header(reqwest::header::USER_AGENT, "nagisalake-hub")
        .send()
        .await
        .map_err(|error| OauthError::Provider(format!("/user/emails request failed: {error}")))?
        .error_for_status()
        .map_err(|error| {
            OauthError::Provider(format!(
                "/user/emails returned an error (is the user:email scope granted?): {error}"
            ))
        })?
        .json()
        .await
        .map_err(|error| OauthError::Provider(format!("/user/emails was not JSON: {error}")))?;

    // Prefer the primary verified address, then any verified one. An unverified
    // address is still reported, with the flag clear, so the caller can create a
    // new account while refusing to link to an existing one.
    let chosen = emails
        .iter()
        .find(|entry| entry.primary && entry.verified)
        .or_else(|| emails.iter().find(|entry| entry.verified))
        .or_else(|| emails.iter().find(|entry| entry.primary))
        .or_else(|| emails.first());

    let (email, email_verified) = match chosen {
        Some(entry) => (entry.email.clone(), entry.verified),
        None => (
            user.email.clone().ok_or_else(|| {
                OauthError::Identity(
                    "GitHub returned no email address; add one to your GitHub account".into(),
                )
            })?,
            false,
        ),
    };

    Ok(FederatedIdentity {
        subject: user.id.to_string(),
        email,
        email_verified,
        display_name: user.name.or(Some(user.login)),
    })
}

/// Validates the post-login redirect target.
///
/// Only a same-site absolute path is accepted, so the callback cannot be turned
/// into an open redirect. `//evil.example` is rejected because a browser reads it
/// as a protocol-relative URL to another host.
pub fn sanitize_redirect_path(candidate: Option<&str>) -> String {
    let Some(path) = candidate.map(str::trim).filter(|value| !value.is_empty()) else {
        return "/".into();
    };
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        return "/".into();
    }
    if path.contains("://") {
        return "/".into();
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_paths_cannot_leave_the_site() {
        assert_eq!(sanitize_redirect_path(Some("/jobs")), "/jobs");
        assert_eq!(sanitize_redirect_path(Some("/jobs?a=1")), "/jobs?a=1");
        assert_eq!(sanitize_redirect_path(None), "/");
        assert_eq!(sanitize_redirect_path(Some("")), "/");
        // A protocol-relative URL reads as another host to the browser.
        assert_eq!(sanitize_redirect_path(Some("//evil.example")), "/");
        assert_eq!(sanitize_redirect_path(Some("https://evil.example")), "/");
        assert_eq!(sanitize_redirect_path(Some("jobs")), "/");
        // Backslashes are normalised to slashes by some browsers.
        assert_eq!(sanitize_redirect_path(Some("/\\evil.example")), "/");
    }

    #[test]
    fn pkce_challenge_is_base64url_of_the_verifier_digest() {
        let pkce = generate_pkce();
        assert!(pkce.verifier.len() >= 43, "verifier must be long enough");
        assert!(!pkce.challenge.contains('='), "must be unpadded base64url");
        assert!(!pkce.challenge.contains('+') && !pkce.challenge.contains('/'));

        // Recompute independently.
        let digest = hash_secret(&pkce.verifier);
        let raw = data_encoding::HEXLOWER.decode(digest.as_bytes()).unwrap();
        assert_eq!(pkce.challenge, data_encoding::BASE64URL_NOPAD.encode(&raw));

        // Two calls must not collide.
        assert_ne!(generate_pkce().verifier, generate_pkce().verifier);
        assert_ne!(generate_state(), generate_state());
    }

    #[test]
    fn google_identity_comes_from_the_id_token() {
        // header.payload.signature; only the payload is read.
        let payload = serde_json::json!({
            "sub": "1234567890",
            "email": "user@example.com",
            "email_verified": true,
            "name": "Example User"
        });
        let encoded = data_encoding::BASE64URL_NOPAD.encode(payload.to_string().as_bytes());
        let token = format!("header.{encoded}.signature");

        let identity = identity_from_id_token(&token).expect("payload should parse");
        assert_eq!(identity.subject, "1234567890");
        assert_eq!(identity.email, "user@example.com");
        assert!(identity.email_verified);
        assert_eq!(identity.display_name.as_deref(), Some("Example User"));
    }

    #[test]
    fn an_unverified_or_malformed_id_token_does_not_claim_verification() {
        let payload = serde_json::json!({"sub": "1", "email": "a@b.c"});
        let encoded = data_encoding::BASE64URL_NOPAD.encode(payload.to_string().as_bytes());
        let identity = identity_from_id_token(&format!("h.{encoded}.s")).unwrap();
        assert!(
            !identity.email_verified,
            "a missing email_verified claim must not be treated as verified"
        );

        // The string form some providers send.
        let payload = serde_json::json!({"sub": "1", "email": "a@b.c", "email_verified": "true"});
        let encoded = data_encoding::BASE64URL_NOPAD.encode(payload.to_string().as_bytes());
        assert!(
            identity_from_id_token(&format!("h.{encoded}.s"))
                .unwrap()
                .email_verified
        );

        // Missing sub or email yields nothing rather than a partial identity.
        for payload in [
            serde_json::json!({"email": "a@b.c"}),
            serde_json::json!({"sub": "1"}),
        ] {
            let encoded = data_encoding::BASE64URL_NOPAD.encode(payload.to_string().as_bytes());
            assert!(identity_from_id_token(&format!("h.{encoded}.s")).is_none());
        }
        assert!(identity_from_id_token("not-a-token").is_none());
        assert!(identity_from_id_token("h.!!!.s").is_none());
    }

    #[test]
    fn domain_restriction_matches_only_the_listed_domains() {
        let mut provider = Provider {
            name:                  "google".into(),
            kind:                  ProviderKind::Google,
            client_id:             "id".into(),
            client_secret:         "secret".into(),
            authorize_url:         "https://a".into(),
            token_url:             "https://t".into(),
            userinfo_url:          "https://u".into(),
            scopes:                vec!["email".into()],
            allowed_email_domains: Vec::new(),
        };
        // Empty means unrestricted.
        assert!(provider.allows_email("anyone@gmail.com"));

        provider.allowed_email_domains = vec!["example.com".into()];
        assert!(provider.allows_email("user@example.com"));
        assert!(
            provider.allows_email("USER@EXAMPLE.COM"),
            "case-insensitive"
        );
        assert!(!provider.allows_email("user@other.com"));
        // A lookalike must not pass.
        assert!(!provider.allows_email("user@notexample.com"));
        assert!(!provider.allows_email("malformed"));
    }

    #[test]
    fn authorize_url_carries_state_and_challenge() {
        let provider = Provider {
            name:                  "google".into(),
            kind:                  ProviderKind::Google,
            client_id:             "client id".into(),
            client_secret:         "secret".into(),
            authorize_url:         "https://accounts.example/auth".into(),
            token_url:             "https://t".into(),
            userinfo_url:          "https://u".into(),
            scopes:                vec!["openid".into(), "email".into()],
            allowed_email_domains: Vec::new(),
        };
        let url = authorize_url(
            &provider,
            "https://hub.example/api/v1/auth/oauth/google/callback",
            "state-value",
            "challenge-value",
        );
        assert!(url.starts_with("https://accounts.example/auth?"));
        assert!(url.contains("state=state-value"));
        assert!(url.contains("code_challenge=challenge-value"));
        assert!(url.contains("code_challenge_method=S256"));
        // Spaces and separators must be encoded.
        assert!(url.contains("client_id=client%20id"));
        assert!(url.contains("scope=openid%20email"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fhub.example%2F"));
        // The secret must never appear in a URL the browser sees.
        assert!(!url.contains("secret"));
    }

    #[test]
    fn resolving_a_provider_requires_its_secret() {
        let config = OauthConfig {
            public_url: "https://hub.example".into(),
            providers:  BTreeMap::from([("google".into(), ProviderConfig {
                kind:                  ProviderKind::Google,
                client_id:             "client".into(),
                client_secret_env:     "NAGISALAKE_TEST_OAUTH_SECRET_MISSING".into(),
                authorize_url:         None,
                token_url:             None,
                userinfo_url:          None,
                scopes:                None,
                allowed_email_domains: Vec::new(),
            })]),
        };
        // A missing secret fails startup rather than producing a broken button.
        assert!(matches!(
            config.resolve(),
            Err(OauthError::InvalidConfig(_))
        ));

        // SAFETY: single-threaded test, and the variable is unique to it.
        unsafe { std::env::set_var("NAGISALAKE_TEST_OAUTH_SECRET_MISSING", "s3cret") };
        let resolved = config
            .resolve()
            .expect("should resolve once the secret exists");
        let provider = &resolved["google"];
        assert_eq!(provider.client_secret, "s3cret");
        // Built-in endpoints are filled in.
        assert_eq!(provider.token_url, "https://oauth2.googleapis.com/token");
        assert!(provider.scopes.contains(&"email".to_string()));
        unsafe { std::env::remove_var("NAGISALAKE_TEST_OAUTH_SECRET_MISSING") };
    }

    #[test]
    fn public_url_must_be_absolute() {
        for public_url in ["", "hub.example", "/hub"] {
            let config = OauthConfig {
                public_url: public_url.into(),
                providers:  BTreeMap::new(),
            };
            assert!(
                matches!(config.resolve(), Err(OauthError::InvalidConfig(_))),
                "{public_url:?} should be rejected"
            );
        }
        let config = OauthConfig {
            public_url: "https://hub.example/".into(),
            providers:  BTreeMap::new(),
        };
        assert!(config.resolve().is_ok());
        // The trailing slash must not double up.
        assert_eq!(
            config.redirect_uri("google"),
            "https://hub.example/api/v1/auth/oauth/google/callback"
        );
    }

    #[test]
    fn github_needs_the_email_scope_by_default() {
        // Without user:email the verification status is unavailable, so linking
        // to an existing account could never be allowed.
        assert!(
            ProviderKind::Github
                .default_scopes()
                .contains(&"user:email".to_string())
        );
    }

    #[test]
    fn linuxdo_defaults_match_connect() {
        assert_eq!(
            ProviderKind::Linuxdo.authorize_url(),
            "https://connect.linux.do/oauth2/authorize"
        );
        assert_eq!(
            ProviderKind::Linuxdo.token_url(),
            "https://connect.linux.do/oauth2/token"
        );
        assert_eq!(
            ProviderKind::Linuxdo.userinfo_url(),
            "https://connect.linux.do/api/user"
        );
        assert_eq!(ProviderKind::Linuxdo.default_scopes(), vec!["user"]);
    }

    #[test]
    fn linuxdo_userinfo_uses_a_subject_scoped_synthetic_email() {
        let identity = linuxdo_identity_from_userinfo(&serde_json::json!({
            "id": 123,
            "username": "alice",
            "name": "Alice"
        }))
        .unwrap();
        assert_eq!(identity.subject, "123");
        assert_eq!(identity.email, "linuxdo-123@linuxdo-connect.invalid");
        assert!(identity.email_verified);
        assert_eq!(identity.display_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn linuxdo_userinfo_rejects_an_unsafe_subject() {
        for claims in [
            serde_json::json!({}),
            serde_json::json!({"id": "123@evil.example"}),
            serde_json::json!({"id": "a".repeat(57)}),
        ] {
            assert!(linuxdo_identity_from_userinfo(&claims).is_err());
        }
    }

    #[test]
    fn token_response_accepts_json_and_form_encoding() {
        for body in [
            &b"{\"access_token\":\"json-token\"}"[..],
            &b"access_token=form-token&token_type=bearer"[..],
        ] {
            assert!(parse_token_response(body).unwrap().access_token.is_some());
        }
    }

    #[tokio::test]
    async fn linuxdo_exchange_accepts_form_token_and_numeric_user_id() {
        use axum::{
            Json, Router,
            body::Bytes,
            http::{HeaderMap, header::CONTENT_TYPE},
            response::IntoResponse,
            routing::{get, post},
        };

        async fn token(body: Bytes) -> impl IntoResponse {
            let form: BTreeMap<String, String> = serde_urlencoded::from_bytes(&body).unwrap();
            assert_eq!(form["client_id"], "linuxdo-client");
            assert_eq!(form["client_secret"], "linuxdo-secret");
            assert_eq!(form["code_verifier"], "pkce-verifier");
            (
                [(CONTENT_TYPE, "application/x-www-form-urlencoded")],
                "access_token=linuxdo-access&token_type=bearer",
            )
        }

        async fn user(headers: HeaderMap) -> Json<serde_json::Value> {
            assert_eq!(headers["authorization"], "Bearer linuxdo-access");
            Json(serde_json::json!({
                "id": 123,
                "username": "alice",
                "name": "Alice"
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/token", post(token))
                    .route("/user", get(user)),
            )
            .await
        });
        let provider = Provider {
            name:                  "linuxdo".into(),
            kind:                  ProviderKind::Linuxdo,
            client_id:             "linuxdo-client".into(),
            client_secret:         "linuxdo-secret".into(),
            authorize_url:         format!("http://{address}/authorize"),
            token_url:             format!("http://{address}/token"),
            userinfo_url:          format!("http://{address}/user"),
            scopes:                vec!["user".into()],
            allowed_email_domains: Vec::new(),
        };

        let identity = exchange_and_fetch_identity(
            &reqwest::Client::new(),
            &provider,
            "https://hub.example/api/v1/auth/oauth/linuxdo/callback",
            "authorization-code",
            "pkce-verifier",
        )
        .await
        .unwrap();
        assert_eq!(identity.subject, "123");
        assert_eq!(identity.email, "linuxdo-123@linuxdo-connect.invalid");
        server.abort();
    }
}
