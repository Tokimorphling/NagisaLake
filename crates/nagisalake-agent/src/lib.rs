//! Provider-independent, bounded skill executions.
//!
//! [`AgentService<B>`] implements `service_async::Service<RunRequest>` using
//! static dispatch. [`Backend`] adds the `Send` future guarantee needed at the
//! Tokio task boundary (the workspace's `Service` trait does not require it).
//! No `dyn Backend`, `async_trait`, or boxed future is needed. HTTP/auth/quota
//! policy belongs to the Hub, not to an upstream agent server.

mod handle;
mod model;
pub mod opencode;
mod service;

pub use handle::{CompletionReceiver, EventSender, RunHandle};
pub use model::{
    AgentError, Completion, Execution, Progress, RunCommand, RunEvent, RunRequest, Skill,
};
pub use service::{AgentService, Backend, RuntimeConfig};

#[cfg(test)]
mod tests;
