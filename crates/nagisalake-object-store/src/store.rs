//! Object storage facade dispatching to the enabled backend.

use crate::{
    config::{S3ObjectStoreConfig, default_presign_ttl_seconds},
    error::ObjectStoreError,
    s3::S3ObjectStore,
};
use nagisalake_protocol::PresignedRequest;
use std::time::Duration;

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
