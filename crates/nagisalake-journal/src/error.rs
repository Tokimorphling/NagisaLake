use nagisalake_core::JobState;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("invalid SQLite URL {url}: {source}")]
    InvalidUrl { url: String, source: sqlx::Error },
    #[error("journal database failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("journal serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("job {0} was not found")]
    NotFound(String),
    #[error("job {0} was redispatched with different immutable fields")]
    DispatchConflict(String),
    #[error("invalid job state {0:?} in journal")]
    InvalidState(String),
    #[error("invalid job transition from {current:?} to {next:?}")]
    InvalidTransition {
        current: JobState,
        next:    JobState,
    },
    #[error("stale job event sequence {received}; current sequence is {current}")]
    StaleEventSequence { current: u64, received: u64 },
}
