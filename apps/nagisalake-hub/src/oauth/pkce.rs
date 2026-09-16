//! Browser-facing URL helpers: PKCE, state, the authorize URL, and redirect
//! sanitisation.

use super::config::{Provider, ProviderKind};

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
    let digest = nagisalake_hub_auth::hash_secret(&verifier);
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
