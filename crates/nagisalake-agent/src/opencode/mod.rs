//! OpenCode HTTP/SSE adapter. One shared upstream event connection, routed by
//! session id; persisted messages are the completion authority.
mod backend;
mod client;
mod config;
mod diagnostics;
mod reconcile;
mod router;
mod sse;
mod wire;

pub use backend::OpenCode;
pub use client::{Health, OpenCodeClient, SkillInfo};
pub use config::{ModelRef, OpenCodeConfig};

#[cfg(test)]
mod tests;
