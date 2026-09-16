//! S3 client operations: presigning, HEAD validation, streaming GET, delete.

use crate::{
    config::{S3ObjectStoreConfig, resolve_secret_env},
    error::ObjectStoreError,
};
use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Builder as S3ConfigBuilder, Credentials, Region},
    presigning::PresigningConfig,
};
use nagisalake_protocol::PresignedRequest;
use std::{collections::BTreeMap, time::Duration};

#[derive(Debug, Clone)]
pub struct S3ObjectStore {
    client:                 Client,
    bucket:                 String,
    pub(super) presign_ttl: Duration,
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

    pub async fn head(&self, key: &str) -> Result<crate::store::ObjectMetadata, ObjectStoreError> {
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
        Ok(crate::store::ObjectMetadata {
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
    ) -> Result<crate::store::ObjectBody, ObjectStoreError> {
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
        Ok(crate::store::ObjectBody {
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

    pub async fn health_check(&self) -> Result<(), ObjectStoreError> {
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map_err(request_error)?;
        Ok(())
    }
}

pub(crate) fn validate_key(key: &str) -> Result<(), ObjectStoreError> {
    if key.trim().is_empty() || key.starts_with('/') || key.contains("..") {
        return Err(ObjectStoreError::InvalidConfig(
            "object key must be a relative canonical path".into(),
        ));
    }
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
