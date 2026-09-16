//! ComfyUI HTTP API errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ComfyUiError {
    #[error("ComfyUI HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("ComfyUI file operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("ComfyUI returned an invalid response: {0}")]
    InvalidResponse(String),
    #[error("ComfyUI rejected workflow nodes: {0}")]
    WorkflowRejected(String),
    #[error("ComfyUI execution failed: {0}")]
    ExecutionFailed(String),
    #[error("ComfyUI wait was cancelled")]
    Cancelled,
}
