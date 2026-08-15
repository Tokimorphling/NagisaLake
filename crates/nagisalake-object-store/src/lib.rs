//! Private S3-compatible object storage for media artifacts.
//!
//! The control plane stores stable bucket/key metadata and issues short-lived
//! requests. It never persists presigned URLs. A streaming read is also exposed
//! for the Hub's authorized same-origin media endpoints.

use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Builder as S3ConfigBuilder, Credentials, Region},
    presigning::PresigningConfig,
};
use nagisalake_protocol::PresignedRequest;
use serde::Deserialize;
use std::{collections::BTreeMap, fmt, time::Duration};
use thiserror::Error;

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

fn default_region() -> String {
    "us-east-1".into()
}

fn default_true() -> bool {
    true
}

fn default_presign_ttl_seconds() -> u64 {
    900
}

#[derive(Debug, Clone)]
pub struct ObjectMetadata {
    pub size_bytes:   u64,
    pub content_type: Option<String>,
    pub sha256:       Option<String>,
}

/// Streaming response used by the Hub without buffering large media objects.
#[derive(Debug)]
pub struct ObjectBody {
    pub body:          aws_sdk_s3::primitives::ByteStream,
    pub size_bytes:    u64,
    pub content_type:  Option<String>,
    pub content_range: Option<String>,
    pub etag:          Option<String>,
}

#[derive(Debug, Error)]
pub enum ObjectStoreError {
    #[error("object storage is disabled")]
    Disabled,
    #[error("invalid object storage configuration: {0}")]
    InvalidConfig(String),
    #[error("object storage request failed: {0}")]
    Request(String),
}

#[derive(Debug, Clone)]
pub enum ObjectStore {
    Disabled,
    S3(S3ObjectStore),
}

impl ObjectStore {
    pub async fn from_s3_config(
        config: Option<S3ObjectStoreConfig>,
    ) -> Result<Self, ObjectStoreError> {
        match config {
            Some(config) => S3ObjectStore::new(config).await.map(Self::S3),
            None => Ok(Self::Disabled),
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::S3(_))
    }

    /// Lifetime of the presigned URLs this store issues.
    ///
    /// Callers use it to bound how long a reserved upload can stay pending: once
    /// the URL expires the client can no longer complete it.
    pub fn presign_ttl(&self) -> Duration {
        match self {
            Self::S3(store) => store.presign_ttl,
            Self::Disabled => Duration::from_secs(default_presign_ttl_seconds()),
        }
    }

    pub async fn presign_put(
        &self,
        key: &str,
        content_type: &str,
        size_bytes: u64,
        sha256: &str,
    ) -> Result<PresignedRequest, ObjectStoreError> {
        match self {
            Self::Disabled => Err(ObjectStoreError::Disabled),
            Self::S3(store) => {
                store
                    .presign_put(key, content_type, size_bytes, sha256)
                    .await
            }
        }
    }

    pub async fn presign_get(&self, key: &str) -> Result<PresignedRequest, ObjectStoreError> {
        match self {
            Self::Disabled => Err(ObjectStoreError::Disabled),
            Self::S3(store) => store.presign_get(key).await,
        }
    }

    pub async fn head(&self, key: &str) -> Result<ObjectMetadata, ObjectStoreError> {
        match self {
            Self::Disabled => Err(ObjectStoreError::Disabled),
            Self::S3(store) => store.head(key).await,
        }
    }

    pub async fn get(
        &self,
        key: &str,
        range: Option<&str>,
    ) -> Result<ObjectBody, ObjectStoreError> {
        match self {
            Self::Disabled => Err(ObjectStoreError::Disabled),
            Self::S3(store) => store.get(key, range).await,
        }
    }

    pub async fn delete(&self, key: &str) -> Result<(), ObjectStoreError> {
        match self {
            Self::Disabled => Err(ObjectStoreError::Disabled),
            Self::S3(store) => store.delete(key).await,
        }
    }

    pub async fn health_check(&self) -> Result<(), ObjectStoreError> {
        match self {
            Self::Disabled => Ok(()),
            Self::S3(store) => store.health_check().await,
        }
    }
}

#[derive(Debug, Clone)]
pub struct S3ObjectStore {
    client:      Client,
    bucket:      String,
    presign_ttl: Duration,
}

impl S3ObjectStore {
    pub async fn new(mut config: S3ObjectStoreConfig) -> Result<Self, ObjectStoreError> {
        if config.bucket.trim().is_empty() {
            return Err(ObjectStoreError::InvalidConfig("bucket is required".into()));
        }
        if !(60..=604_800).contains(&config.presign_ttl_seconds) {
            return Err(ObjectStoreError::InvalidConfig(
                "presign_ttl_seconds must be between 60 and 604800".into(),
            ));
        }
        resolve_secret_env(
            &mut config.access_key_id,
            config.access_key_id_env.as_deref(),
            "access key",
        )?;
        resolve_secret_env(
            &mut config.secret_access_key,
            config.secret_access_key_env.as_deref(),
            "secret access key",
        )?;
        resolve_secret_env(
            &mut config.session_token,
            config.session_token_env.as_deref(),
            "session token",
        )?;
        let credentials = match (
            config.access_key_id.as_deref(),
            config.secret_access_key.as_deref(),
        ) {
            (Some(access_key), Some(secret_key))
                if !access_key.trim().is_empty() && !secret_key.trim().is_empty() =>
            {
                Some(Credentials::new(
                    access_key,
                    secret_key,
                    config.session_token,
                    None,
                    "nagisalake-config",
                ))
            }
            (None, None) => {
                return Err(ObjectStoreError::InvalidConfig(
                    "access_key_id and secret_access_key are required".into(),
                ));
            }
            _ => {
                return Err(ObjectStoreError::InvalidConfig(
                    "access_key_id and secret_access_key must be set together".into(),
                ));
            }
        };

        let mut builder = S3ConfigBuilder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region));
        if let Some(endpoint_url) = config
            .endpoint_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            builder = builder.endpoint_url(endpoint_url);
        }
        let s3_config = builder
            .credentials_provider(credentials.expect("validated credentials"))
            .force_path_style(config.force_path_style)
            .build();
        Ok(Self {
            client:      Client::from_conf(s3_config),
            bucket:      config.bucket,
            presign_ttl: Duration::from_secs(config.presign_ttl_seconds),
        })
    }

    pub async fn presign_put(
        &self,
        key: &str,
        content_type: &str,
        size_bytes: u64,
        sha256: &str,
    ) -> Result<PresignedRequest, ObjectStoreError> {
        validate_key(key)?;
        if size_bytes == 0 || size_bytes > 5 * 1024 * 1024 * 1024 {
            return Err(ObjectStoreError::InvalidConfig(
                "single PUT object size must be between 1 byte and 5 GiB".into(),
            ));
        }
        let request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .metadata("sha256", sha256.to_ascii_lowercase())
            .presigned(presign_config(self.presign_ttl)?)
            .await
            .map_err(request_error)?;
        Ok(presigned_request(
            request.method(),
            request.uri(),
            request
                .headers()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
                .collect(),
            self.presign_ttl,
        ))
    }

    pub async fn presign_get(&self, key: &str) -> Result<PresignedRequest, ObjectStoreError> {
        validate_key(key)?;
        let request = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presign_config(self.presign_ttl)?)
            .await
            .map_err(request_error)?;
        Ok(presigned_request(
            request.method(),
            request.uri(),
            request
                .headers()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
                .collect(),
            self.presign_ttl,
        ))
    }

    pub async fn head(&self, key: &str) -> Result<ObjectMetadata, ObjectStoreError> {
        validate_key(key)?;
        let output = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(request_error)?;
        let size_bytes = output
            .content_length()
            .and_then(|size| u64::try_from(size).ok())
            .ok_or_else(|| ObjectStoreError::Request("invalid S3 content length".into()))?;
        Ok(ObjectMetadata {
            size_bytes,
            content_type: output.content_type().map(str::to_string),
            sha256: output
                .metadata()
                .and_then(|metadata| metadata.get("sha256"))
                .cloned(),
        })
    }

    pub async fn get(
        &self,
        key: &str,
        range: Option<&str>,
    ) -> Result<ObjectBody, ObjectStoreError> {
        validate_key(key)?;
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .set_range(range.map(str::to_owned))
            .send()
            .await
            .map_err(request_error)?;
        let size_bytes = output
            .content_length()
            .and_then(|size| u64::try_from(size).ok())
            .ok_or_else(|| ObjectStoreError::Request("invalid S3 content length".into()))?;
        Ok(ObjectBody {
            content_type: output.content_type().map(str::to_owned),
            content_range: output.content_range().map(str::to_owned),
            etag: output.e_tag().map(str::to_owned),
            size_bytes,
            body: output.body,
        })
    }

    pub async fn delete(&self, key: &str) -> Result<(), ObjectStoreError> {
        validate_key(key)?;
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(request_error)?;
        Ok(())
    }

    async fn health_check(&self) -> Result<(), ObjectStoreError> {
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map_err(request_error)?;
        Ok(())
    }
}

fn validate_key(key: &str) -> Result<(), ObjectStoreError> {
    if key.trim().is_empty() || key.starts_with('/') || key.contains("..") {
        return Err(ObjectStoreError::InvalidConfig(
            "object key must be a relative canonical path".into(),
        ));
    }
    Ok(())
}

fn resolve_secret_env(
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

fn presign_config(ttl: Duration) -> Result<PresigningConfig, ObjectStoreError> {
    PresigningConfig::expires_in(ttl).map_err(request_error)
}

fn presigned_request(
    method: &str,
    uri: &str,
    headers: BTreeMap<String, String>,
    ttl: Duration,
) -> PresignedRequest {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    PresignedRequest {
        method: method.into(),
        url: uri.to_string(),
        headers,
        expires_at_unix_ms: now.saturating_add(ttl).as_millis().min(i64::MAX as u128) as i64,
    }
}

fn request_error(error: impl std::fmt::Display) -> ObjectStoreError {
    ObjectStoreError::Request(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_keys() {
        assert!(validate_key("media/user/object.png").is_ok());
        assert!(validate_key("/absolute").is_err());
        assert!(validate_key("media/../secret").is_err());
    }

    #[test]
    fn debug_output_redacts_static_credentials() {
        let config = S3ObjectStoreConfig {
            bucket:                "private".into(),
            region:                default_region(),
            endpoint_url:          None,
            access_key_id:         Some("visible-access-key".into()),
            access_key_id_env:     None,
            secret_access_key:     Some("visible-secret-key".into()),
            secret_access_key_env: None,
            session_token:         Some("visible-session-token".into()),
            session_token_env:     None,
            force_path_style:      true,
            presign_ttl_seconds:   default_presign_ttl_seconds(),
        };

        let output = format!("{config:?}");
        assert!(!output.contains("visible-access-key"));
        assert!(!output.contains("visible-secret-key"));
        assert!(!output.contains("visible-session-token"));
        assert!(output.contains("[redacted]"));
    }
}
