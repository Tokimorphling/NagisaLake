use super::*;

pub(super) fn require_consumer(headers: &HeaderMap, config: &HubConfig) -> Result<(), HubError> {
    let token = bearer_token(headers)
        .ok_or_else(|| HubError::Unauthorized("consumer bearer token is required".into()))?;
    let expected = config
        .auth
        .consumer_token
        .as_deref()
        .ok_or_else(|| HubError::InvalidConfig("consumer token is not configured".into()))?;
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Err(HubError::Unauthorized("invalid consumer token".into()));
    }
    Ok(())
}

pub(super) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .trim()
                .strip_prefix("Bearer ")
                .or_else(|| value.trim().strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

pub(super) fn validate_artifact_metadata(
    name: &str,
    content_type: &str,
    size_bytes: u64,
    sha256: &str,
    max_bytes: u64,
) -> Result<(), HubError> {
    if name.trim().is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(HubError::InvalidRequest("artifact name is invalid".into()));
    }
    if content_type.trim().is_empty() {
        return Err(HubError::InvalidRequest(
            "content_type must not be empty".into(),
        ));
    }
    if size_bytes == 0 || size_bytes > max_bytes {
        return Err(HubError::InvalidRequest(
            "artifact size is outside the configured limit".into(),
        ));
    }
    if !is_sha256(sha256) {
        return Err(HubError::InvalidRequest(
            "sha256 must be a 64-character hexadecimal digest".into(),
        ));
    }
    Ok(())
}

pub(super) fn object_metadata_matches(metadata: &ObjectMetadata, artifact: &ArtifactView) -> bool {
    metadata.size_bytes == artifact.size_bytes
        && metadata.content_type.as_deref() == Some(artifact.content_type.as_str())
        && metadata
            .sha256
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(&artifact.sha256))
}

pub(super) fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn safe_filename(value: &str) -> String {
    let candidate = std::path::Path::new(value)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact.bin");
    let result: String = candidate
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '.' | '-' | '_') {
                value
            } else {
                '_'
            }
        })
        .collect();
    if result.is_empty() || result == "." || result == ".." {
        "artifact.bin".into()
    } else {
        result
    }
}

/// Defence in depth for worker id components.
///
/// `Register::validate` already rejects anything outside this character set
/// before a worker id is built, so on the live path this is the identity
/// function. It stays as a guard for any future caller that has not been
/// validated: substituting characters here would silently merge two distinct
/// identities onto one id, so validation, not sanitisation, is the real
/// boundary.
pub(super) fn safe_component(value: &str) -> String {
    let result = value
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '.' | '-' | '_') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    if result.is_empty() {
        "worker".into()
    } else {
        result
    }
}

pub(super) fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(super) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

pub(super) fn api_error(error: HubError) -> Response {
    let status = match &error {
        HubError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        HubError::NotFound(_) => StatusCode::NOT_FOUND,
        HubError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        HubError::Conflict(_) => StatusCode::CONFLICT,
        HubError::Unavailable(_) | HubError::ObjectStore(_) => StatusCode::BAD_GATEWAY,
        HubError::Forbidden(_) => StatusCode::FORBIDDEN,
        HubError::QuotaExceeded(_) | HubError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
        HubError::InvalidConfig(_) => StatusCode::INTERNAL_SERVER_ERROR,
        HubError::ConfigIo { .. }
        | HubError::ConfigParse(_)
        | HubError::Store(_)
        | HubError::Io(_)
        | HubError::Axum(_) => StatusCode::INTERNAL_SERVER_ERROR,
        HubError::Transport(_) => StatusCode::BAD_GATEWAY,
    };
    (
        status,
        Json(json!({
            "error": {
                "type": "nagisalake_hub_error",
                "message": truncate(&error.to_string(), 1_000),
            }
        })),
    )
        .into_response()
}

/// Hub errors returned by API and lifecycle boundaries.
#[derive(Debug, Error)]
pub enum HubError {
    #[error("failed to read config {path}: {source}")]
    ConfigIo {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    ConfigParse(#[from] toml::de::Error),
    #[error("invalid Hub configuration: {0}")]
    InvalidConfig(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("worker unavailable: {0}")]
    Unavailable(String),
    #[error("object store failed: {0}")]
    ObjectStore(String),
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),
    #[error("too many requests; retry in {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },
    #[error("persistent store failed: {0}")]
    Store(#[from] StoreError),
    #[error("transport failed: {0}")]
    Transport(#[from] TransportError),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP server failed: {0}")]
    Axum(#[from] axum::Error),
}
