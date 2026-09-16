//! Private S3-compatible object storage for media artifacts.
//!
//! The control plane stores stable bucket/key metadata and issues short-lived
//! requests. It never persists presigned URLs. A streaming read is also exposed
//! for the Hub's authorized same-origin media endpoints.
//!
//! ## Key Components
//!
//! - [`ObjectStore`]: facade dispatching to the enabled backend.
//! - [`S3ObjectStore`]: presigning, HEAD validation, and streaming reads.

mod config;
mod error;
mod s3;
mod store;

pub use config::S3ObjectStoreConfig;
pub use error::ObjectStoreError;
pub use s3::S3ObjectStore;
pub use store::{ObjectBody, ObjectMetadata, ObjectStore};

#[cfg(test)]
mod tests;
