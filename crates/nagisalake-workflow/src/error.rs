//! Workflow catalog and dispatch validation errors.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("at least one workflow must be installed")]
    EmptyCatalog,
    #[error("failed to read workflow template {path}: {source}")]
    ReadTemplate {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse workflow template {path}: {source}")]
    ParseTemplate {
        path:   PathBuf,
        source: serde_json::Error,
    },
    #[error("workflow {0} must be a ComfyUI API JSON object")]
    TemplateMustBeObject(String),
    #[error("invalid ComfyUI editor workflow: {0}")]
    InvalidUiWorkflow(String),
    #[error("workflow field {0} must not be empty")]
    Required(&'static str),
    #[error("JSON pointer {0:?} does not exist in the workflow template")]
    InvalidPointer(String),
    #[error("duplicate input binding index {0}")]
    DuplicateInputIndex(usize),
    #[error("duplicate public workflow input name {0:?}")]
    DuplicatePublicInput(String),
    #[error("workflow bindings {first:?} and {second:?} overlap")]
    OverlappingPointers { first: String, second: String },
    #[error("input binding indices must be contiguous and start at zero")]
    NonContiguousInputs,
    #[error("duplicate workflow {id} version {version}")]
    DuplicateWorkflow { id: String, version: String },
    #[error("workflow {id} version {version} is not installed")]
    NotInstalled { id: String, version: String },
    #[error("job parameters must be a JSON object")]
    ParametersMustBeObject,
    #[error("parameter {0:?} is not allowlisted by the workflow")]
    UnknownParameter(String),
    #[error("workflow expects {expected} ordered inputs, got {actual}")]
    InputCount { expected: usize, actual: usize },
}
