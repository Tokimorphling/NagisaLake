//! SQLite durable outbox; state validation is shared with the in-memory journal.
use crate::{
    JournalError,
    state::{parse_state, same_dispatch_identity, state_str, validate_event_sequence},
};
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
use std::str::FromStr;

#[cfg(test)]
mod tests;

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
            .foreign_keys(true)
            // Keep the journal durable across OS/power failure: replay cannot
            // recover an event whose local durable copy was lost. WAL reduces
            // reader/writer interference without weakening commit durability.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Full)
            .busy_timeout(std::time::Duration::from_secs(5));
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
        // Deliberately not `decode_job`: this path only needs the state machine
        // fields. Selecting and parsing `dispatch_json` here deserialized the
        // whole workflow `parameters` value on every job event, inside the
        // transaction that serializes all other journal writes.
        let current = sqlx::query(
            "SELECT state, event_sequence, pending_event_json FROM worker_jobs WHERE job_id = ?",
        )
        .bind(&request.event.job_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| JournalError::NotFound(request.event.job_id.clone()))
        .and_then(decode_event_head)?;
        validate_event_sequence(
            current.event_sequence,
            current.pending_event.as_ref(),
            &request.event,
        )?;
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

/// The subset of a journal row the event path actually reads.
///
/// Kept separate from [`JobRecord`] so the hot event write never pays for
/// parsing the dispatch payload.
struct EventHead {
    state:          JobState,
    event_sequence: u64,
    pending_event:  Option<nagisalake_protocol::JobEvent>,
}

fn decode_event_head(row: SqliteRow) -> Result<EventHead, JournalError> {
    let pending_event_json: Option<String> = row.try_get("pending_event_json")?;
    let sequence: i64 = row.try_get("event_sequence")?;
    Ok(EventHead {
        state:          parse_state(row.try_get("state")?)?,
        event_sequence: sequence.max(0) as u64,
        pending_event:  pending_event_json
            .map(|payload| serde_json::from_str(&payload))
            .transpose()?,
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
