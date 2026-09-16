//! Allowlisted ComfyUI API workflow catalog.
//!
//! ## Key Components
//!
//! - [`WorkflowCatalog`]: loads API-format templates, validates RFC 6901
//!   bindings, and renders only allowlisted parameters and artifact inputs.
//! - [`WorkflowService`]: motore/service-async facade over the catalog.
//! - `normalize`: best-effort ComfyUI editor-graph conversion with warnings.

mod catalog;
mod error;
mod manifest;
mod normalize;
mod pointer;
mod service;

pub use catalog::{InputBinding, WorkflowCatalog, WorkflowConfig};
pub use error::WorkflowError;
pub use service::{RenderWorkflow, WorkflowService, WorkflowServiceFactory};

#[cfg(test)]
mod tests;
