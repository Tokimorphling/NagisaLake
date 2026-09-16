//! Composable services for the ComfyUI HTTP API.
//!
//! ## Key Components
//!
//! - [`ComfyUiService`]: one motore service per ComfyUI HTTP endpoint.
//! - [`PollUntilCompleteService`]: history polling with queue inspection.
//! - [`build_service`]: stacks both into the shape the runtime executes.

mod config;
mod error;
mod parse;
mod poll;
mod service;

pub use config::{BuildError, ComfyUiConfig, ComfyUiStackConfig};
pub use error::ComfyUiError;
pub use poll::{
    PollUntilCompleteFactory, PollUntilCompleteService, WaitForCompletion, build_service,
};
pub use service::ComfyUiService;

#[cfg(test)]
mod tests;
