use crate::{quota::release_job_for_terminal_tx, rows::*, *};
use sqlx::{AssertSqlSafe, query, query_as};

const INITIAL_DISPATCH_DELAY_MS: i64 = 5_000;

impl PgStore {
    pub async fn create_job(&self, input: JobUpsert<'_>) -> Result<(), StoreError> {
        query(
            "INSERT INTO jobs \
             (organization_id,id,actor_id,actor_kind,actor_user_id,workflow_id,workflow_version,\
             parameters_json,input_artifact_ids_json,output_artifact_ids_json,worker_id,\
             worker_organization_id,session_id,attempt,state,progress,prompt_id,error,last_event,\
             created_at,updated_at) VALUES \
             ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$20)",
        )
        .bind(input.organization_id)
        .bind(input.id)
        .bind(input.actor_id)
        .bind(input.actor_kind)
        .bind(input.actor_user_id)
        .bind(input.workflow_id)
        .bind(input.workflow_version)
        .bind(input.parameters_json)
        .bind(input.input_artifact_ids_json)
        .bind(input.output_artifact_ids_json)
        .bind(input.worker_id)
        .bind(input.worker_organization_id)
        .bind(input.session_id)
        .bind(input.attempt)
        .bind(input.state)
        .bind(input.progress)
        .bind(input.prompt_id)
        .bind(input.error)
        .bind(input.last_event)
        .bind(input.now)
        .execute(&self.pool)
        .await?;
        query(
            "INSERT INTO dispatch_outbox (organization_id,job_id,attempt,status,available_at) \
             VALUES ($1,$2,$3,'pending',$4) ON CONFLICT (organization_id,job_id,attempt) DO \
             NOTHING",
        )
        .bind(input.organization_id)
        .bind(input.id)
        .bind(input.attempt)
        .bind(input.now.saturating_add(INITIAL_DISPATCH_DELAY_MS))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomically reserves job quota, claims input artifacts, writes the job,
    /// its idempotency record, and the durable dispatch outbox entry.
    pub async fn commit_new_job(
        &self,
        input: JobUpsert<'_>,
        input_artifact_ids: &[String],
        idempotency: Option<IdempotencyInsert<'_>>,
        device_admission: Option<DeviceUseAdmission<'_>>,
    ) -> Result<CommitJobResult, StoreError> {
        let mut tx = self.pool.begin().await?;
        // Idempotent retries are common when a client times out. Resolve them
        // before touching quota rows so a replay never waits behind another
        // tenant submission.
        if let Some(idempotency) = idempotency.as_ref() {
            let existing = query_as::<_, IdempotencyRow>(
                "SELECT request_hash,job_id FROM idempotency_records WHERE organization_id=$1 AND \
                 actor_kind=$2 AND actor_id=$3 AND endpoint=$4 AND idempotency_key=$5",
            )
            .bind(idempotency.organization_id)
            .bind(idempotency.actor_kind)
            .bind(idempotency.actor_id)
            .bind(idempotency.endpoint)
            .bind(idempotency.key)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(existing) = existing {
                if existing.request_hash != idempotency.request_hash {
                    return Err(StoreError::Conflict(
                        "idempotency key was used for a different request".into(),
                    ));
                }
                tx.commit().await?;
                return Ok(CommitJobResult::Existing {
                    job_id: existing.job_id,
                });
            }
        }
        // The policy is immutable during normal submissions, so a shared row
        // lock is enough. Only the usage row needs an exclusive lock because
        // it is the single atomic counter being updated below.
        let quota = query_as::<_, (i64, i64, i64)>(
            "SELECT max_concurrent_jobs,max_jobs_per_period,period_seconds FROM quota_policies \
             WHERE organization_id=$1 FOR SHARE",
        )
        .bind(input.organization_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("quota policy".into()))?;
        let usage = query_as::<_, (i64, i64, i64)>(
            "SELECT active_jobs,period_jobs,period_started_at FROM quota_usage WHERE \
             organization_id=$1 FOR UPDATE",
        )
        .bind(input.organization_id)
        .fetch_one(&mut *tx)
        .await?;
        // Two identical requests may have passed the read-only check before
        // either transaction committed. Recheck after the usage lock so the
        // loser returns the durable existing job instead of a unique-key
        // database error.
        if let Some(idempotency) = idempotency.as_ref() {
            let existing = query_as::<_, IdempotencyRow>(
                "SELECT request_hash,job_id FROM idempotency_records WHERE organization_id=$1 AND \
                 actor_kind=$2 AND actor_id=$3 AND endpoint=$4 AND idempotency_key=$5",
            )
            .bind(idempotency.organization_id)
            .bind(idempotency.actor_kind)
            .bind(idempotency.actor_id)
            .bind(idempotency.endpoint)
            .bind(idempotency.key)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(existing) = existing {
                if existing.request_hash != idempotency.request_hash {
                    return Err(StoreError::Conflict(
                        "idempotency key was used for a different request".into(),
                    ));
                }
                tx.commit().await?;
                return Ok(CommitJobResult::Existing {
                    job_id: existing.job_id,
                });
            }
        }
        if let Some(admission) = device_admission.as_ref() {
            crate::devices::enforce_device_use_policy_tx(&mut tx, admission).await?;
        }
        let period_jobs = if input.now.saturating_sub(usage.2) >= quota.2.saturating_mul(1_000) {
            0
        } else {
            usage.1
        };
        if usage.0 >= quota.0 || period_jobs >= quota.1 {
            return Err(StoreError::QuotaExceeded("jobs".into()));
        }
        for artifact_id in input_artifact_ids {
            let result = query(
                "UPDATE artifacts SET job_id=$1,updated_at=$2 WHERE organization_id=$3 AND id=$4 \
                 AND state='ready' AND job_id IS NULL",
            )
            .bind(input.id)
            .bind(input.now)
            .bind(input.organization_id)
            .bind(artifact_id)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() != 1 {
                return Err(StoreError::Conflict(format!(
                    "input artifact {artifact_id} is not ready or was claimed concurrently"
                )));
            }
        }
        query(
            "INSERT INTO jobs \
             (organization_id,id,actor_id,actor_kind,actor_user_id,workflow_id,workflow_version,\
             parameters_json,input_artifact_ids_json,output_artifact_ids_json,worker_id,\
             worker_organization_id,session_id,attempt,state,progress,prompt_id,error,last_event,\
             created_at,updated_at) VALUES \
             ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$20)",
        )
        .bind(input.organization_id)
        .bind(input.id)
        .bind(input.actor_id)
        .bind(input.actor_kind)
        .bind(input.actor_user_id)
        .bind(input.workflow_id)
        .bind(input.workflow_version)
        .bind(input.parameters_json)
        .bind(input.input_artifact_ids_json)
        .bind(input.output_artifact_ids_json)
        .bind(input.worker_id)
        .bind(input.worker_organization_id)
        .bind(input.session_id)
        .bind(input.attempt)
        .bind(input.state)
        .bind(input.progress)
        .bind(input.prompt_id)
        .bind(input.error)
        .bind(input.last_event)
        .bind(input.now)
        .execute(&mut *tx)
        .await?;
        query(
            "UPDATE quota_usage SET \
             active_jobs=active_jobs+1,period_jobs=$1,period_started_at=CASE WHEN $2=0 THEN $3 \
             ELSE period_started_at END,updated_at=$3 WHERE organization_id=$4",
        )
        .bind(period_jobs + 1)
        .bind(period_jobs)
        .bind(input.now)
        .bind(input.organization_id)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO dispatch_outbox (organization_id,job_id,attempt,status,available_at) \
             VALUES ($1,$2,$3,'pending',$4)",
        )
        .bind(input.organization_id)
        .bind(input.id)
        .bind(input.attempt)
        .bind(input.now.saturating_add(INITIAL_DISPATCH_DELAY_MS))
        .execute(&mut *tx)
        .await?;
        if let Some(idempotency) = idempotency {
            query(
                "INSERT INTO idempotency_records \
                 (organization_id,actor_kind,actor_id,endpoint,idempotency_key,request_hash,\
                 job_id,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(idempotency.organization_id)
            .bind(idempotency.actor_kind)
            .bind(idempotency.actor_id)
            .bind(idempotency.endpoint)
            .bind(idempotency.key)
            .bind(idempotency.request_hash)
            .bind(input.id)
            .bind(idempotency.now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(CommitJobResult::Created)
    }

    pub async fn job(
        &self,
        organization_id: &str,
        id: &str,
    ) -> Result<Option<StoredJob>, StoreError> {
        Ok(query_as::<_, JobRow>(
            "SELECT organization_id,id,actor_id,actor_kind,actor_user_id,workflow_id,\
             workflow_version,parameters_json,input_artifact_ids_json,output_artifact_ids_json,\
             worker_id,worker_organization_id,session_id,attempt,state,progress,prompt_id,error,\
             last_event,created_at,updated_at FROM jobs WHERE organization_id=$1 AND id=$2",
        )
        .bind(organization_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn jobs_for_org(&self, organization_id: &str) -> Result<Vec<StoredJob>, StoreError> {
        Ok(query_as::<_, JobRow>(
            "SELECT organization_id,id,actor_id,actor_kind,actor_user_id,workflow_id,\
             workflow_version,parameters_json,input_artifact_ids_json,output_artifact_ids_json,\
             worker_id,worker_organization_id,session_id,attempt,state,progress,prompt_id,error,\
             last_event,created_at,updated_at FROM jobs WHERE organization_id=$1 ORDER BY \
             created_at DESC",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// Returns recovered worker-journal entries that the Hub has already made
    /// terminal. The worker organization is deliberately separate from a job's
    /// owning organization: shared devices execute jobs belonging to another
    /// tenant.
    ///
    /// Callers pass only ids reported by the worker's own durable journal. The
    /// result can therefore be sent back as targeted `CancelJob` commands
    /// without broadcasting every historical terminal job for the device.
    pub async fn terminal_recovery_jobs_for_worker(
        &self,
        worker_organization_id: &str,
        worker_id: &str,
        recovery_job_ids: &[String],
    ) -> Result<Vec<String>, StoreError> {
        if recovery_job_ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(query_as::<_, (String,)>(
            "SELECT DISTINCT id FROM jobs WHERE worker_organization_id=$1 AND worker_id=$2 AND \
             id=ANY($3) AND state IN ('completed','failed','cancelled') ORDER BY id",
        )
        .bind(worker_organization_id)
        .bind(worker_id)
        .bind(recovery_job_ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect())
    }

    /// One page of an organization's jobs, newest first.
    ///
    /// Keyset rather than OFFSET: the cursor is the last row's
    /// `(created_at, id)`, so cost stays flat however deep the caller pages.
    /// `id` is in the key because jobs created in the same millisecond would
    /// otherwise be skipped or repeated across pages. Served by
    /// `idx_jobs_org_created_id` as an index-only scan.
    pub async fn jobs_page(
        &self,
        organization_id: &str,
        limit: i64,
        after: Option<(i64, &str)>,
    ) -> Result<Vec<StoredJob>, StoreError> {
        const COLUMNS: &str = "organization_id,id,actor_id,actor_kind,actor_user_id,workflow_id,\
                               workflow_version,parameters_json,input_artifact_ids_json,\
                               output_artifact_ids_json,worker_id,worker_organization_id,\
                               session_id,attempt,state,progress,prompt_id,error,last_event,\
                               created_at,updated_at";
        let rows = match after {
            Some((created_at, id)) => {
                query_as::<_, JobRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM jobs WHERE organization_id=$1 AND (created_at, id) < \
                     ($2, $3) ORDER BY created_at DESC, id DESC LIMIT $4"
                )))
                .bind(organization_id)
                .bind(created_at)
                .bind(id)
                .bind(limit.max(1))
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                query_as::<_, JobRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM jobs WHERE organization_id=$1 ORDER BY created_at \
                     DESC, id DESC LIMIT $2"
                )))
                .bind(organization_id)
                .bind(limit.max(1))
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn all_jobs(&self) -> Result<Vec<StoredJob>, StoreError> {
        Ok(query_as::<_, JobRow>(
            "SELECT organization_id,id,actor_id,actor_kind,actor_user_id,workflow_id,\
             workflow_version,parameters_json,input_artifact_ids_json,output_artifact_ids_json,\
             worker_id,worker_organization_id,session_id,attempt,state,progress,prompt_id,error,\
             last_event,created_at,updated_at FROM jobs ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// Jobs that have not reached a terminal state.
    ///
    /// This is what a restarting Hub actually needs resident: a terminal job
    /// carries no scheduling decisions. Loading every job instead made startup
    /// scale with total history rather than with work in flight.
    pub async fn unfinished_jobs(&self) -> Result<Vec<StoredJob>, StoreError> {
        Ok(query_as::<_, JobRow>(
            "SELECT organization_id,id,actor_id,actor_kind,actor_user_id,workflow_id,\
             workflow_version,parameters_json,input_artifact_ids_json,output_artifact_ids_json,\
             worker_id,worker_organization_id,session_id,attempt,state,progress,prompt_id,error,\
             last_event,created_at,updated_at FROM jobs WHERE state NOT IN \
             ('completed','failed','cancelled') ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// Events belonging to jobs that are still unfinished.
    ///
    /// Paired with [`Self::unfinished_jobs`] so hydration does not read the
    /// event history of jobs it is not loading.
    pub async fn events_for_unfinished_jobs(&self) -> Result<Vec<StoredJobEvent>, StoreError> {
        Ok(query_as::<_, EventRow>(
            "SELECT e.organization_id,e.job_id,e.attempt,e.sequence,e.kind,e.progress,e.prompt_id,\
             e.message,e.unix_ms FROM job_events e JOIN jobs j ON j.organization_id = \
             e.organization_id AND j.id = e.job_id WHERE j.state NOT IN \
             ('completed','failed','cancelled') ORDER BY e.job_id, e.sequence",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// Artifacts still awaiting their upload.
    ///
    /// A `ready` artifact is only read when someone downloads it, so it is
    /// fetched on demand instead of being held resident.
    pub async fn pending_artifacts(&self) -> Result<Vec<StoredArtifact>, StoreError> {
        Ok(query_as::<_, ArtifactRow>(
            "SELECT organization_id,id,job_id,name,content_type,size_bytes,sha256,state,\
             object_key,created_at,updated_at FROM artifacts WHERE state='pending_upload' ORDER \
             BY created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn rebind_job_session(
        &self,
        organization_id: &str,
        job_id: &str,
        attempt: i64,
        expected_session_id: &str,
        new_session_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        let result = query(
            "UPDATE jobs SET session_id=$1,updated_at=GREATEST(updated_at,$2) WHERE \
             organization_id=$3 AND id=$4 AND attempt=$5 AND state NOT IN \
             ('completed','failed','cancelled') AND (session_id=$6 OR session_id=$1)",
        )
        .bind(new_session_id)
        .bind(now)
        .bind(organization_id)
        .bind(job_id)
        .bind(attempt)
        .bind(expected_session_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn append_job_output_artifact(
        &self,
        organization_id: &str,
        job_id: &str,
        attempt: i64,
        artifact_id: &str,
        session_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        let result = query(
            "UPDATE jobs SET output_artifact_ids_json=CASE WHEN output_artifact_ids_json::jsonb ? \
             $1 THEN output_artifact_ids_json ELSE (output_artifact_ids_json::jsonb || \
             jsonb_build_array($1::text))::text END,updated_at=GREATEST(updated_at,$2) WHERE \
             organization_id=$3 AND id=$4 AND attempt=$5 AND session_id=$6",
        )
        .bind(artifact_id)
        .bind(now)
        .bind(organization_id)
        .bind(job_id)
        .bind(attempt)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn update_job_if_current(
        &self,
        input: ConditionalJobUpdate<'_>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let result = query(
            "UPDATE jobs SET state=COALESCE($1,state),error=$2,updated_at=GREATEST(updated_at,$3) \
             WHERE organization_id=$4 AND id=$5 AND attempt=$6 AND state=$7 AND last_event=$8",
        )
        .bind(input.state)
        .bind(input.error)
        .bind(input.now)
        .bind(input.organization_id)
        .bind(input.id)
        .bind(input.attempt)
        .bind(input.expected_state)
        .bind(input.expected_last_event)
        .execute(&mut *tx)
        .await?;
        let updated = result.rows_affected() == 1;
        if updated && matches!(input.state, Some("completed" | "failed" | "cancelled")) {
            release_job_for_terminal_tx(&mut tx, input.organization_id, input.id, input.now)
                .await?;
        }
        tx.commit().await?;
        Ok(updated)
    }

    pub async fn apply_job_event(
        &self,
        event: EventInsert<'_>,
        update: JobEventUpdate<'_>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let event_result = query(
            "INSERT INTO job_events \
             (organization_id,job_id,attempt,sequence,kind,progress,prompt_id,message,unix_ms,\
             created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT \
             (organization_id,job_id,attempt,sequence) DO UPDATE SET sequence=job_events.sequence \
             WHERE job_events.kind=EXCLUDED.kind AND job_events.progress IS NOT DISTINCT FROM \
             EXCLUDED.progress AND job_events.prompt_id IS NOT DISTINCT FROM EXCLUDED.prompt_id \
             AND job_events.message=EXCLUDED.message AND job_events.unix_ms=EXCLUDED.unix_ms",
        )
        .bind(event.organization_id)
        .bind(event.job_id)
        .bind(event.attempt)
        .bind(event.sequence)
        .bind(event.kind)
        .bind(event.progress)
        .bind(event.prompt_id)
        .bind(event.message)
        .bind(event.unix_ms)
        .bind(event.now)
        .execute(&mut *tx)
        .await?;
        if event_result.rows_affected() != 1 {
            tx.rollback().await?;
            return Err(StoreError::Conflict(
                "job event sequence was reused with a different payload".into(),
            ));
        }
        let result = query(
            "UPDATE jobs SET \
             session_id=$1,state=$2,progress=COALESCE($3,progress),prompt_id=COALESCE($4,\
             prompt_id),error=$5,last_event=GREATEST(last_event,$6),\
             updated_at=GREATEST(updated_at,$7) WHERE organization_id=$8 AND id=$9 AND \
             attempt=$10 AND state=$11 AND last_event=$12 AND (session_id=$13 OR session_id=$1)",
        )
        .bind(update.session_id)
        .bind(update.state)
        .bind(event.progress)
        .bind(event.prompt_id)
        .bind(update.error)
        .bind(event.sequence)
        .bind(event.now)
        .bind(event.organization_id)
        .bind(event.job_id)
        .bind(event.attempt)
        .bind(update.expected_state)
        .bind(update.expected_last_event)
        .bind(update.expected_session_id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        if matches!(update.state, "completed" | "failed" | "cancelled") {
            release_job_for_terminal_tx(&mut tx, event.organization_id, event.job_id, event.now)
                .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    pub async fn events_for_job(
        &self,
        organization_id: &str,
        job_id: &str,
    ) -> Result<Vec<StoredJobEvent>, StoreError> {
        Ok(query_as::<_, EventRow>(
            "SELECT organization_id,job_id,attempt,sequence,kind,progress,prompt_id,message,\
             unix_ms FROM job_events WHERE organization_id=$1 AND job_id=$2 ORDER BY sequence",
        )
        .bind(organization_id)
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn events_for_org(
        &self,
        organization_id: &str,
    ) -> Result<Vec<StoredJobEvent>, StoreError> {
        Ok(query_as::<_, EventRow>(
            "SELECT organization_id,job_id,attempt,sequence,kind,progress,prompt_id,message,\
             unix_ms FROM job_events WHERE organization_id=$1 ORDER BY job_id,sequence",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn all_events(&self) -> Result<Vec<StoredJobEvent>, StoreError> {
        Ok(query_as::<_, EventRow>(
            "SELECT organization_id,job_id,attempt,sequence,kind,progress,prompt_id,message,\
             unix_ms FROM job_events ORDER BY job_id,sequence",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn idempotency(
        &self,
        organization_id: &str,
        actor_kind: &str,
        actor_id: &str,
        endpoint: &str,
        key: &str,
    ) -> Result<Option<IdempotencyResult>, StoreError> {
        Ok(query_as::<_, IdempotencyRow>(
            "SELECT request_hash,job_id FROM idempotency_records WHERE organization_id=$1 AND \
             actor_kind=$2 AND actor_id=$3 AND endpoint=$4 AND idempotency_key=$5",
        )
        .bind(organization_id)
        .bind(actor_kind)
        .bind(actor_id)
        .bind(endpoint)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?
        .map(|value| IdempotencyResult {
            request_hash: value.request_hash,
            job_id:       value.job_id,
        }))
    }

    pub async fn mark_dispatch_delivered(
        &self,
        organization_id: &str,
        job_id: &str,
        attempt: i64,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE dispatch_outbox SET status='delivered',claimed_at=$1,last_error=NULL WHERE \
             organization_id=$2 AND job_id=$3 AND attempt=$4",
        )
        .bind(now_unix_ms())
        .bind(organization_id)
        .bind(job_id)
        .bind(attempt)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Claims pending dispatches for a local consumer. A short lease makes a
    /// crashed Hub's claims visible to the next instance without requiring a
    /// second recovery table.
    pub async fn claim_dispatches(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<DispatchOutbox>, StoreError> {
        let rows = query_as::<_, (String, String, i64)>(
            "WITH candidates AS (SELECT organization_id,job_id,attempt FROM dispatch_outbox WHERE \
             (status='pending' AND available_at <= $1) OR (status='claimed' AND claimed_at IS NOT \
             NULL AND claimed_at <= $1-30000) ORDER BY available_at LIMIT $2 FOR UPDATE SKIP \
             LOCKED) UPDATE dispatch_outbox d SET status='claimed',claimed_at=$1 FROM candidates \
             c WHERE d.organization_id=c.organization_id AND d.job_id=c.job_id AND \
             d.attempt=c.attempt RETURNING d.organization_id,d.job_id,d.attempt",
        )
        .bind(now)
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(organization_id, job_id, attempt)| DispatchOutbox {
                organization_id,
                job_id,
                attempt,
            })
            .collect())
    }

    pub async fn record_dispatch_error(
        &self,
        organization_id: &str,
        job_id: &str,
        attempt: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE dispatch_outbox SET \
             status='pending',attempts=attempts+1,available_at=$1,last_error=$2 WHERE \
             organization_id=$3 AND job_id=$4 AND attempt=$5",
        )
        .bind(now_unix_ms().saturating_add(5_000))
        .bind(error)
        .bind(organization_id)
        .bind(job_id)
        .bind(attempt)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn put_idempotency(
        &self,
        input: IdempotencyInsert<'_>,
    ) -> Result<Option<IdempotencyResult>, StoreError> {
        let existing = query_as::<_, IdempotencyRow>(
            "SELECT request_hash,job_id FROM idempotency_records WHERE organization_id=$1 AND \
             actor_kind=$2 AND actor_id=$3 AND endpoint=$4 AND idempotency_key=$5",
        )
        .bind(input.organization_id)
        .bind(input.actor_kind)
        .bind(input.actor_id)
        .bind(input.endpoint)
        .bind(input.key)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(existing) = existing {
            return Ok(Some(IdempotencyResult {
                request_hash: existing.request_hash,
                job_id:       existing.job_id,
            }));
        }
        query(
            "INSERT INTO idempotency_records \
             (organization_id,actor_kind,actor_id,endpoint,idempotency_key,request_hash,job_id,\
             created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING",
        )
        .bind(input.organization_id)
        .bind(input.actor_kind)
        .bind(input.actor_id)
        .bind(input.endpoint)
        .bind(input.key)
        .bind(input.request_hash)
        .bind(input.job_id)
        .bind(input.now)
        .execute(&self.pool)
        .await?;
        Ok(None)
    }
}
