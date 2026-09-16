//! Worker lifecycle errors.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("failed to read worker config {path}: {source}")]
    ConfigIo {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse worker config: {0}")]
    ConfigParse(#[from] toml::de::Error),
    #[error("invalid worker config: {0}")]
    InvalidConfig(String),
    #[error("workflow catalog failed: {0}")]
    Workflow(String),
    #[error("journal failed: {0}")]
    Journal(String),
    #[error("ComfyUI service failed: {0}")]
    Comfy(String),
    #[error("runtime failed: {0}")]
    Runtime(String),
    #[error("transport failed: {0}")]
    Transport(#[from] nagisalake_transport::TransportError),
    #[error("registration timed out")]
    RegistrationTimeout,
    #[error("Hub rejected registration: {0}")]
    RegistrationFailed(String),
    #[error("unexpected Hub message: {0}")]
    UnexpectedMessage(String),
    #[error("worker I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
