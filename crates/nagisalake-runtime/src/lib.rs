//! Tokio worker runtime for durable ComfyUI jobs.
//!
//! The runtime deliberately separates the control plane from the data plane:
//! JSON messages and acknowledgements use [`WorkerRuntime`], while input and
//! output bytes are streamed directly between the worker, ComfyUI, and the
//! presigned object-store URLs carried by the protocol.
//!
//! ## Key Components
//!
//! - [`WorkerRuntime`]: reconnect-aware control message bus and ACK waiters.
//! - [`JobRunner`]: bounded, resumable ComfyUI execution state machine.
//! - [`WorkerExecutionConfig`]: local filesystem and polling limits.
//!
//! ## Features
//!
//! - Tokio-native cancellation and concurrency limits.
//! - SQLite journal outbox replay for job events.
//! - Streaming input/output transfers with size and SHA-256 checks.
//! - No binary data is written to the control protocol.

mod artifacts;
mod bus;
mod error;
mod runner;
mod util;

pub use bus::{JobSlot, WorkerRuntime};
pub use error::{HttpFailure, RuntimeError};
pub use runner::{JobRunner, WorkerExecutionConfig};

#[cfg(test)]
mod tests;
