//! S3 connection settings with redacted Debug output and secret resolution.

use crate::error::ObjectStoreError;
use serde::Deserialize;
use std::fmt;

#[derive(Clone, Deserialize)]
pub struct S3ObjectStoreConfig {
    pub bucket:                String,
    #[serde(default = "default_region")]
    pub region:                String,
    #[serde(default)]
    pub endpoint_url:          Option<String>,
    #[serde(default)]
    pub access_key_id:         Option<String>,
    #[serde(default)]
    pub access_key_id_env:     Option<String>,
    #[serde(default)]
    pub secret_access_key:     Option<String>,
    #[serde(default)]
    pub secret_access_key_env: Option<String>,
    #[serde(default)]
    pub session_token:         Option<String>,
    #[serde(default)]
    pub session_token_env:     Option<String>,
    #[serde(default = "default_true")]
    pub force_path_style:      bool,
    #[serde(default = "default_presign_ttl_seconds")]
    pub presign_ttl_seconds:   u64,
}

impl fmt::Debug for S3ObjectStoreConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("S3ObjectStoreConfig")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint_url", &self.endpoint_url)
            .field(
                "access_key_id",
                &self.access_key_id.as_ref().map(|_| "[redacted]"),
            )
            .field("access_key_id_env", &self.access_key_id_env)
            .field(
                "secret_access_key",
                &self.secret_access_key.as_ref().map(|_| "[redacted]"),
            )
            .field("secret_access_key_env", &self.secret_access_key_env)
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[redacted]"),
            )
            .field("session_token_env", &self.session_token_env)
            .field("force_path_style", &self.force_path_style)
            .field("presign_ttl_seconds", &self.presign_ttl_seconds)
            .finish()
    }
}

pub(crate) fn default_region() -> String {
    "us-east-1".into()
}

fn default_true() -> bool {
    true
}

pub(crate) fn default_presign_ttl_seconds() -> u64 {
    900
}

pub(crate) fn resolve_secret_env(
    value: &mut Option<String>,
    env_name: Option<&str>,
    label: &str,
) -> Result<(), ObjectStoreError> {
    if value
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(());
    }
    let Some(env_name) = env_name.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(());
    };
    let resolved = std::env::var(env_name).map_err(|error| {
        ObjectStoreError::InvalidConfig(format!("read {label} env {env_name}: {error}"))
    })?;
    if resolved.trim().is_empty() {
        return Err(ObjectStoreError::InvalidConfig(format!(
            "{label} env {env_name} is empty"
        )));
    }
    *value = Some(resolved);
    Ok(())
}
