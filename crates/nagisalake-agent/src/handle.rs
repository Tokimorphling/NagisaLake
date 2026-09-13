use crate::{AgentError, Progress, RunCommand, RunEvent};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Bounded progress channel supplied to each backend.
#[derive(Clone)]
pub struct EventSender {
    pub(crate) sender:         mpsc::Sender<RunEvent>,
    pub(crate) text_bytes:     Arc<AtomicUsize>,
    pub(crate) max_text_bytes: usize,
}

impl EventSender {
    pub async fn send(&self, event: Progress) -> Result<(), AgentError> {
        if let Progress::TextDelta { text } = &event {
            self.text_bytes
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bytes| {
                    bytes
                        .checked_add(text.len())
                        .filter(|&total| total <= self.max_text_bytes)
                })
                .map_err(|_| AgentError::OutputLimit)?;
        }
        self.sender
            .send(event.into())
            .await
            .map_err(|_| AgentError::Cancelled)
    }
}

/// Independent terminal observer for durable history/metrics. Observing does
/// not keep a disconnected consumer alive or prevent cancellation on drop.
#[derive(Clone)]
pub struct CompletionReceiver {
    pub(crate) receiver: watch::Receiver<Option<Arc<RunEvent>>>,
}

impl CompletionReceiver {
    pub async fn wait(&mut self) -> Result<Arc<RunEvent>, AgentError> {
        loop {
            if let Some(event) = self.receiver.borrow_and_update().clone() {
                return Ok(event);
            }
            self.receiver
                .changed()
                .await
                .map_err(|_| AgentError::Protocol("execution task ended without an outcome"))?;
        }
    }
}

/// The one downstream event stream for a run. Dropping it cancels execution.
/// The terminal event uses a separate watch channel so a full progress queue
/// cannot swallow a cancellation, timeout or completed result.
pub struct RunHandle {
    pub(crate) id:           String,
    pub(crate) receiver:     mpsc::Receiver<RunEvent>,
    pub(crate) completion:   CompletionReceiver,
    pub(crate) cancellation: CancellationToken,
    pub(crate) ended:        bool,
}

impl RunHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn completion(&self) -> CompletionReceiver {
        self.completion.clone()
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn command(&self, command: RunCommand) {
        match command {
            RunCommand::Cancel => self.cancellation.cancel(),
        }
    }

    pub fn cancel(&self) {
        self.command(RunCommand::Cancel);
    }

    pub async fn recv(&mut self) -> Option<RunEvent> {
        if self.ended {
            return None;
        }
        if let Some(event) = self.receiver.recv().await {
            return Some(event);
        }
        self.ended = true;
        Some(match self.completion.wait().await {
            Ok(event) => (*event).clone(),
            Err(error) => RunEvent::Error {
                execution_id: self.id.clone(),
                code:         error.code().into(),
                message:      error.to_string(),
            },
        })
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
