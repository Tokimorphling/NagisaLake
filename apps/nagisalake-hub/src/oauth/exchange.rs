//! Token exchange and per-provider identity fetchers.

use super::config::{FederatedIdentity, OauthError, Provider, ProviderKind};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: Option<String>,
    error:                   Option<String>,
    error_description:       Option<String>,
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

pub(super) fn parse_token_response(body: &[u8]) -> Result<TokenResponse, OauthError> {
    serde_json::from_slice(body)
        .or_else(|_| serde_urlencoded::from_bytes(body))
        .map_err(|error| {
            OauthError::Provider(format!("token response was not JSON or form data: {error}"))
        })
}

/// Test helper for claim-shape compatibility. The live path reads userinfo with
/// the provider-issued access token, avoiding an incomplete local JWT verifier.
#[cfg(test)]
pub(crate) fn identity_from_id_token(id_token: &str) -> Option<FederatedIdentity> {
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

pub(super) fn linuxdo_identity_from_userinfo(
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
