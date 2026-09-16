//! Versioned control protocol between Nagisalake hubs and ComfyUI workers.
//!
//! The control plane carries JSON metadata only. Artifact bytes move through
//! short-lived presigned requests and never through the Tokilake control
//! stream.
//!
//! ## Key Components
//!
//! - [`WorkerMessage`] / [`HubMessage`]: the tagged control envelopes.
//! - [`Validate`]: frame-level checks applied before any message is trusted.

mod messages;
mod validate;

pub use messages::*;
pub use validate::{MAX_IDENTITY_CHARS, Validate, ValidationError};

#[cfg(test)]
mod tests;
