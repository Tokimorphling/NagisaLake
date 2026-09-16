//! S3-compatible object storage errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ObjectStoreError {
    #[error("object storage is disabled")]
    Disabled,
    #[error("invalid object storage configuration: {0}")]
    InvalidConfig(String),
    #[error("object storage request failed: {0}")]
    Request(String),
}
