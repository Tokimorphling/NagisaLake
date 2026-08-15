//! Durable and in-memory worker journal services.

use nagisalake_core::{
    ClearPendingEvent, GetJob, JobRecord, JobState, ListUnfinished, SetJobState, SetPendingEvent,
    SetPromptId, UpsertDispatch,
};
use nagisalake_protocol::DispatchJob;
use service_async::Service;
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};
use std::{collections::BTreeMap, str::FromStr, sync::Arc};
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct SqliteJournal {
    pool: SqlitePool,
}

impl SqliteJournal {
    pub async fn open(url: &str) -> Result<Self, JournalError> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|source| JournalError::InvalidUrl {
                url: url.into(),
                source,
            })?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS worker_jobs (job_id TEXT PRIMARY KEY, dispatch_json TEXT \
             NOT NULL, state TEXT NOT NULL, prompt_id TEXT NULL, event_sequence BIGINT NOT NULL \
             DEFAULT 0, pending_event_json TEXT NULL, updated_at BIGINT NOT NULL)",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    async fn upsert(&self, dispatch: DispatchJob) -> Result<JobRecord, JournalError> {
        let payload = serde_json::to_string(&dispatch)?;
        let mut transaction = self.pool.begin().await?;
        let existing = sqlx::query(
            "SELECT dispatch_json, state, prompt_id, event_sequence, pending_event_json FROM \
             worker_jobs WHERE job_id = ?",
        )
        .bind(&dispatch.job_id)
        .fetch_optional(&mut *transaction)
        .await?
        .map(decode_job)
        .transpose()?;
        let record = if let Some(mut record) = existing {
            if !same_dispatch_identity(&record.dispatch, &dispatch) {
                return Err(JournalError::DispatchConflict(dispatch.job_id));
            }
            if !record.state.is_terminal() {
                sqlx::query(
                    "UPDATE worker_jobs SET dispatch_json = ?, updated_at = ? WHERE job_id = ?",
                )
                .bind(payload)
                .bind(now_unix_ms())
                .bind(&dispatch.job_id)
                .execute(&mut *transaction)
                .await?;
                record.dispatch = dispatch;
            }
            record
        } else {
            sqlx::query(
                "INSERT INTO worker_jobs (job_id, dispatch_json, state, event_sequence, \
                 updated_at) VALUES (?, ?, 'received', 0, ?)",
            )
            .bind(&dispatch.job_id)
            .bind(payload)
            .bind(now_unix_ms())
            .execute(&mut *transaction)
            .await?;
            JobRecord::received(dispatch)
        };
        transaction.commit().await?;
        Ok(record)
    }

    async fn set_state(&self, request: SetJobState) -> Result<(), JournalError> {
        let mut transaction = self.pool.begin().await?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT state FROM worker_jobs WHERE job_id = ?")
                .bind(&request.job_id)
                .fetch_optional(&mut *transaction)
                .await?;
        let current = current
            .ok_or_else(|| JournalError::NotFound(request.job_id.clone()))
            .and_then(|value| parse_state(&value))?;
        if !current.can_transition_to(request.state) {
            return Err(JournalError::InvalidTransition {
                current,
                next: request.state,
            });
        }
        sqlx::query("UPDATE worker_jobs SET state = ?, updated_at = ? WHERE job_id = ?")
            .bind(state_str(request.state))
            .bind(now_unix_ms())
            .bind(request.job_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }
}

impl Service<UpsertDispatch> for SqliteJournal {
    type Response = JobRecord;
    type Error = JournalError;

    async fn call(&self, request: UpsertDispatch) -> Result<Self::Response, Self::Error> {
        self.upsert(request.0).await
    }
}

impl Service<GetJob> for SqliteJournal {
    type Response = Option<JobRecord>;
    type Error = JournalError;

    async fn call(&self, request: GetJob) -> Result<Self::Response, Self::Error> {
        sqlx::query(
            "SELECT dispatch_json, state, prompt_id, event_sequence, pending_event_json FROM \
             worker_jobs WHERE job_id = ?",
        )
        .bind(request.0)
        .fetch_optional(&self.pool)
        .await?
        .map(decode_job)
        .transpose()
    }
}

impl Service<ListUnfinished> for SqliteJournal {
    type Response = Vec<JobRecord>;
    type Error = JournalError;

    async fn call(&self, _request: ListUnfinished) -> Result<Self::Response, Self::Error> {
        let rows = sqlx::query(
            "SELECT dispatch_json, state, prompt_id, event_sequence, pending_event_json FROM \
             worker_jobs WHERE state NOT IN ('completed', 'failed', 'cancelled') OR \
             pending_event_json IS NOT NULL ORDER BY updated_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(decode_job).collect()
    }
}

impl Service<SetJobState> for SqliteJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetJobState) -> Result<Self::Response, Self::Error> {
        self.set_state(request).await
    }
}

impl Service<SetPromptId> for SqliteJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetPromptId) -> Result<Self::Response, Self::Error> {
        let result =
            sqlx::query("UPDATE worker_jobs SET prompt_id = ?, updated_at = ? WHERE job_id = ?")
                .bind(request.prompt_id)
                .bind(now_unix_ms())
                .bind(&request.job_id)
                .execute(&self.pool)
                .await?;
        ensure_updated(&request.job_id, result.rows_affected())
    }
}

impl Service<SetPendingEvent> for SqliteJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetPendingEvent) -> Result<Self::Response, Self::Error> {
        let mut transaction = self.pool.begin().await?;
        let current = sqlx::query(
            "SELECT dispatch_json, state, prompt_id, event_sequence, pending_event_json FROM \
             worker_jobs WHERE job_id = ?",
        )
        .bind(&request.event.job_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| JournalError::NotFound(request.event.job_id.clone()))
        .and_then(decode_job)?;
        validate_event_sequence(&current, &request.event)?;
        let payload = serde_json::to_string(&request.event)?;
        let sequence = i64::try_from(request.event.sequence).unwrap_or(i64::MAX);
        let result = if let Some(state) = request.state {
            if !current.state.can_transition_to(state) {
                return Err(JournalError::InvalidTransition {
                    current: current.state,
                    next:    state,
                });
            }
            sqlx::query(
                "UPDATE worker_jobs SET state = ?, event_sequence = ?, pending_event_json = ?, \
                 updated_at = ? WHERE job_id = ?",
            )
            .bind(state_str(state))
            .bind(sequence)
            .bind(payload)
            .bind(now_unix_ms())
            .bind(&request.event.job_id)
            .execute(&mut *transaction)
            .await?
        } else {
            sqlx::query(
                "UPDATE worker_jobs SET event_sequence = ?, pending_event_json = ?, updated_at = \
                 ? WHERE job_id = ?",
            )
            .bind(sequence)
            .bind(payload)
            .bind(now_unix_ms())
            .bind(&request.event.job_id)
            .execute(&mut *transaction)
            .await?
        };
        ensure_updated(&request.event.job_id, result.rows_affected())?;
        transaction.commit().await?;
        Ok(())
    }
}

impl Service<ClearPendingEvent> for SqliteJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: ClearPendingEvent) -> Result<Self::Response, Self::Error> {
        sqlx::query(
            "UPDATE worker_jobs SET pending_event_json = NULL, updated_at = ? WHERE job_id = ? \
             AND event_sequence = ?",
        )
        .bind(now_unix_ms())
        .bind(request.job_id)
        .bind(i64::try_from(request.sequence).unwrap_or(i64::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryJournal {
    records: Arc<RwLock<BTreeMap<String, JobRecord>>>,
}

impl Service<UpsertDispatch> for MemoryJournal {
    type Response = JobRecord;
    type Error = JournalError;

    async fn call(&self, request: UpsertDispatch) -> Result<Self::Response, Self::Error> {
        let mut records = self.records.write().await;
        if let Some(record) = records.get_mut(&request.0.job_id) {
            if !same_dispatch_identity(&record.dispatch, &request.0) {
                return Err(JournalError::DispatchConflict(request.0.job_id));
            }
            if !record.state.is_terminal() {
                record.dispatch = request.0;
            }
            return Ok(record.clone());
        }
        let record = JobRecord::received(request.0);
        records.insert(record.dispatch.job_id.clone(), record.clone());
        Ok(record)
    }
}

impl Service<GetJob> for MemoryJournal {
    type Response = Option<JobRecord>;
    type Error = JournalError;

    async fn call(&self, request: GetJob) -> Result<Self::Response, Self::Error> {
        Ok(self.records.read().await.get(&request.0).cloned())
    }
}

impl Service<ListUnfinished> for MemoryJournal {
    type Response = Vec<JobRecord>;
    type Error = JournalError;

    async fn call(&self, _request: ListUnfinished) -> Result<Self::Response, Self::Error> {
        Ok(self
            .records
            .read()
            .await
            .values()
            .filter(|record| !record.state.is_terminal() || record.pending_event.is_some())
            .cloned()
            .collect())
    }
}

impl Service<SetJobState> for MemoryJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetJobState) -> Result<Self::Response, Self::Error> {
        let mut records = self.records.write().await;
        let record = records
            .get_mut(&request.job_id)
            .ok_or_else(|| JournalError::NotFound(request.job_id.clone()))?;
        if !record.state.can_transition_to(request.state) {
            return Err(JournalError::InvalidTransition {
                current: record.state,
                next:    request.state,
            });
        }
        record.state = request.state;
        Ok(())
    }
}

impl Service<SetPromptId> for MemoryJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetPromptId) -> Result<Self::Response, Self::Error> {
        self.records
            .write()
            .await
            .get_mut(&request.job_id)
            .ok_or_else(|| JournalError::NotFound(request.job_id.clone()))?
            .prompt_id = Some(request.prompt_id);
        Ok(())
    }
}

impl Service<SetPendingEvent> for MemoryJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetPendingEvent) -> Result<Self::Response, Self::Error> {
        let mut records = self.records.write().await;
        let record = records
            .get_mut(&request.event.job_id)
            .ok_or_else(|| JournalError::NotFound(request.event.job_id.clone()))?;
        validate_event_sequence(record, &request.event)?;
        if let Some(state) = request.state {
            if !record.state.can_transition_to(state) {
                return Err(JournalError::InvalidTransition {
                    current: record.state,
                    next:    state,
                });
            }
            record.state = state;
        }
        record.event_sequence = request.event.sequence;
        record.pending_event = Some(request.event);
        Ok(())
    }
}

impl Service<ClearPendingEvent> for MemoryJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: ClearPendingEvent) -> Result<Self::Response, Self::Error> {
        let mut records = self.records.write().await;
        if let Some(record) = records.get_mut(&request.job_id)
            && record.event_sequence == request.sequence
        {
            record.pending_event = None;
        }
        Ok(())
    }
}

fn validate_event_sequence(
    record: &JobRecord,
    event: &nagisalake_protocol::JobEvent,
) -> Result<(), JournalError> {
    if event.sequence < record.event_sequence
        || (event.sequence == record.event_sequence && record.pending_event.as_ref() != Some(event))
    {
        Err(JournalError::StaleEventSequence {
            current:  record.event_sequence,
            received: event.sequence,
        })
    } else {
        Ok(())
    }
}

fn same_dispatch_identity(previous: &DispatchJob, next: &DispatchJob) -> bool {
    previous.job_id == next.job_id
        && previous.attempt == next.attempt
        && previous.workflow_id == next.workflow_id
        && previous.workflow_version == next.workflow_version
        && previous.parameters == next.parameters
        && previous.inputs.len() == next.inputs.len()
        && previous
            .inputs
            .iter()
            .zip(&next.inputs)
            .all(|(left, right)| {
                left.artifact_id == right.artifact_id
                    && left.name == right.name
                    && left.content_type == right.content_type
                    && left.size_bytes == right.size_bytes
                    && left.sha256.eq_ignore_ascii_case(&right.sha256)
            })
}

fn decode_job(row: SqliteRow) -> Result<JobRecord, JournalError> {
    let dispatch_json: String = row.try_get("dispatch_json")?;
    let pending_event_json: Option<String> = row.try_get("pending_event_json")?;
    let sequence: i64 = row.try_get("event_sequence")?;
    Ok(JobRecord {
        dispatch:       serde_json::from_str(&dispatch_json)?,
        state:          parse_state(row.try_get("state")?)?,
        prompt_id:      row.try_get("prompt_id")?,
        event_sequence: sequence.max(0) as u64,
        pending_event:  pending_event_json
            .map(|payload| serde_json::from_str(&payload))
            .transpose()?,
    })
}

const fn state_str(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Received => "received",
        JobState::Accepted => "accepted",
        JobState::Running => "running",
        JobState::Uploading => "uploading",
        JobState::Completed => "completed",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn parse_state(value: &str) -> Result<JobState, JournalError> {
    match value {
        "queued" => Ok(JobState::Queued),
        "received" => Ok(JobState::Received),
        "accepted" => Ok(JobState::Accepted),
        "running" => Ok(JobState::Running),
        "uploading" => Ok(JobState::Uploading),
        "completed" => Ok(JobState::Completed),
        "failed" => Ok(JobState::Failed),
        "cancelled" => Ok(JobState::Cancelled),
        other => Err(JournalError::InvalidState(other.into())),
    }
}

fn ensure_updated(job_id: &str, rows_affected: u64) -> Result<(), JournalError> {
    if rows_affected == 0 {
        Err(JournalError::NotFound(job_id.into()))
    } else {
        Ok(())
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use nagisalake_protocol::{JobInput, PresignedRequest};

    fn dispatch(command_id: &str, url: &str) -> DispatchJob {
        DispatchJob {
            command_id:       command_id.into(),
            job_id:           "job-1".into(),
            attempt:          1,
            workflow_id:      "image-edit".into(),
            workflow_version: "v1".into(),
            parameters:       serde_json::json!({"prompt":"hello"}),
            inputs:           vec![JobInput {
                artifact_id:  "input-1".into(),
                name:         "source.png".into(),
                content_type: "image/png".into(),
                size_bytes:   42,
                sha256:       "a".repeat(64),
                download:     PresignedRequest {
                    method:             "GET".into(),
                    url:                url.into(),
                    headers:            BTreeMap::new(),
                    expires_at_unix_ms: 1,
                },
            }],
        }
    }

    #[tokio::test]
    async fn sqlite_retry_refreshes_ephemeral_fields_only() {
        let journal = SqliteJournal::open("sqlite::memory:").await.unwrap();
        journal
            .call(UpsertDispatch(dispatch(
                "command-1",
                "https://objects/first",
            )))
            .await
            .unwrap();
        let refreshed = journal
            .call(UpsertDispatch(dispatch(
                "command-2",
                "https://objects/second",
            )))
            .await
            .unwrap();
        assert_eq!(refreshed.dispatch.command_id, "command-2");
        assert_eq!(
            refreshed.dispatch.inputs[0].download.url,
            "https://objects/second"
        );
        let mut conflicting = dispatch("command-3", "https://objects/third");
        conflicting.parameters = serde_json::json!({"prompt":"changed"});
        assert!(journal.call(UpsertDispatch(conflicting)).await.is_err());
    }

    #[tokio::test]
    async fn memory_and_sqlite_enforce_the_same_state_machine() {
        let journal = MemoryJournal::default();
        journal
            .call(UpsertDispatch(dispatch("command", "https://objects/input")))
            .await
            .unwrap();
        let result = journal
            .call(SetJobState {
                job_id: "job-1".into(),
                state:  JobState::Completed,
            })
            .await;
        assert!(matches!(
            result,
            Err(JournalError::InvalidTransition { .. })
        ));
    }

    #[tokio::test]
    async fn stale_events_cannot_replace_the_pending_outbox_event() {
        let journal = MemoryJournal::default();
        journal
            .call(UpsertDispatch(dispatch("command", "https://objects/input")))
            .await
            .unwrap();
        let newer = nagisalake_protocol::JobEvent {
            job_id:    "job-1".into(),
            attempt:   1,
            sequence:  2,
            kind:      nagisalake_protocol::JobEventKind::Accepted,
            progress:  Some(0.0),
            prompt_id: None,
            message:   String::new(),
            unix_ms:   2,
        };
        journal
            .call(SetPendingEvent {
                event: newer.clone(),
                state: Some(JobState::Accepted),
            })
            .await
            .unwrap();
        let mut older = newer;
        older.sequence = 1;
        assert!(matches!(
            journal
                .call(SetPendingEvent {
                    event: older,
                    state: None,
                })
                .await,
            Err(JournalError::StaleEventSequence { .. })
        ));
    }
}
