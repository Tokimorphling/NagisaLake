//! Embeddable Tokio worker lifecycle for Nagisalake.
//!
//! The worker connects outbound to a Hub, advertises its local workflow
//! catalog, and executes accepted dispatches against one ComfyUI instance.
//! See [`WorkerConfig`] for the configuration shape used by the CLI.
//!
//! ## Key Components
//!
//! - [`WorkerConfig`]: TOML shape, path resolution, and startup validation.
//! - [`Worker`]: reconnect loop, registration, and dispatch admission.

mod config;
mod error;
#[cfg(feature = "python")]
mod python;
mod worker;

pub use config::{
    HubConfig, HubTlsConfig, MAX_QUEUE_DEPTH, StateConfig, WorkerConfig, WorkerIdentity,
};
pub use error::WorkerError;
pub use worker::Worker;

#[cfg(test)]
mod tests;
