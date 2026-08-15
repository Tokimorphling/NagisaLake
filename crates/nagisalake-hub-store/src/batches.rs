use crate::{StoreError, models::*, rows::*, *};
use sqlx::query_as;

/// Input for creating a Batch and all its child jobs in one transaction.
pub struct BatchInsert<'a> {
    pub batch_id:                &'a str,
    pub organization_id:         &'a str,
    pub actor_id:                &'a str,
    pub actor_kind:              &'a str,
    pub actor_user_id:           Option<&'a str>,
    pub workflow_id:             &'a str,
    pub workflow_version:        &'a str,
    pub workflow_content_digest: Option<&'a str>,
    pub base_parameters_json:    &'a str,
    pub variation_spec_json:     &'a str,
    pub device_organization_id:  Option<&'a str>,
    pub device_id:               Option<&'a str>,
    pub total_jobs:              i64,
    pub retry_of_batch_id:       Option<&'a str>,
}

/// One child job in a batch creation transaction.
pub struct BatchChildJob<'a> {
    pub job_id:             &'a str,
    pub batch_index:        i64,
    pub client_item_id:     Option<&'a str>,
    pub parameters_json:    &'a str,
    pub input_artifact_ids: &'a [String],
}

/// Idempotency record for batch creation.
pub struct BatchIdempotencyInsert<'a> {
    pub organization_id: &'a str,
    pub actor_kind:      &'a str,
    pub actor_id:        &'a str,
    pub endpoint:        &'a str,
    pub key:             &'a str,
    pub request_hash:    &'a str,
    pub batch_id:        &'a str,
}

/// The result of a batch creation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitBatchResult {
    Created,
    Existing { batch_id: String },
}

impl PgStore {
    /// Creates a Batch, all child jobs, input artifact links, and dispatch
    /// queue entries in one all-or-nothing transaction.
    ///
    /// Quota is checked and consumed atomically: `inflight += N` and
    /// `period += N`. If any check fails the entire transaction rolls back
    /// and no partial state is left behind.
    pub async fn commit_new_batch(
        &self,
        batch: BatchInsert<'_>,
        children: &[BatchChildJob<'_>],
        shared_input_artifact_ids: &[String],
        idempotency: Option<BatchIdempotencyInsert<'_>>,
        device_admission: Option<DeviceUseAdmission<'_>>,
        now: i64,
    ) -> Result<CommitBatchResult, StoreError> {
        let mut tx = self.pool.begin().await?;

        // Check batch idempotency before touching quota rows. A retry should
        // be a cheap read even when another batch is reserving capacity.
        if let Some(idem) = &idempotency {
            let existing: Option<(String,)> = sqlx::query_as(
                "SELECT batch_id FROM job_batch_idempotency_records WHERE organization_id = $1 \
                 AND actor_kind = $2 AND actor_id = $3 AND endpoint = $4 AND idempotency_key = $5",
            )
            .bind(idem.organization_id)
            .bind(idem.actor_kind)
            .bind(idem.actor_id)
            .bind(idem.endpoint)
            .bind(idem.key)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some((existing_batch_id,)) = existing {
                // Same key — check hash to detect conflict.
                let stored_hash: (String,) = sqlx::query_as(
                    "SELECT request_hash FROM job_batch_idempotency_records WHERE organization_id \
                     = $1 AND actor_kind = $2 AND actor_id = $3 AND endpoint = $4 AND \
                     idempotency_key = $5",
                )
                .bind(idem.organization_id)
                .bind(idem.actor_kind)
                .bind(idem.actor_id)
                .bind(idem.endpoint)
                .bind(idem.key)
                .fetch_one(&mut *tx)
                .await?;
                if stored_hash.0.as_str() != idem.request_hash {
                    return Err(StoreError::Conflict(
                        "idempotency key conflict: request hash mismatch".into(),
                    ));
                }
                return Ok(CommitBatchResult::Existing {
                    batch_id: existing_batch_id,
                });
            }
        }

        // Policy readers can share the row; quota_usage is the only row this
        // transaction mutates and therefore the only exclusive quota lock.
        let policy: QuotaPolicyRow =
            sqlx::query_as("SELECT * FROM quota_policies WHERE organization_id = $1 FOR SHARE")
                .bind(batch.organization_id)
                .fetch_one(&mut *tx)
                .await?;
        let usage: QuotaUsageRow =
            sqlx::query_as("SELECT * FROM quota_usage WHERE organization_id = $1 FOR UPDATE")
                .bind(batch.organization_id)
                .fetch_one(&mut *tx)
                .await?;

        // Close the race where two retries both observed an empty idempotency
        // table before either transaction committed its record.
        if let Some(idem) = &idempotency {
            let existing: Option<(String, String)> = sqlx::query_as(
                "SELECT batch_id,request_hash FROM job_batch_idempotency_records WHERE \
                 organization_id = $1 AND actor_kind = $2 AND actor_id = $3 AND endpoint = $4 AND \
                 idempotency_key = $5",
            )
            .bind(idem.organization_id)
            .bind(idem.actor_kind)
            .bind(idem.actor_id)
            .bind(idem.endpoint)
            .bind(idem.key)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some((existing_batch_id, stored_hash)) = existing {
                if stored_hash != idem.request_hash {
                    return Err(StoreError::Conflict(
                        "idempotency key conflict: request hash mismatch".into(),
                    ));
                }
                tx.commit().await?;
                return Ok(CommitBatchResult::Existing {
                    batch_id: existing_batch_id,
                });
            }
        }

        if let Some(admission) = device_admission.as_ref() {
            crate::devices::enforce_device_use_policy_tx(&mut tx, admission).await?;
        }

        // Compute effective period usage (reset if expired).
        let period_started_at = usage.period_started_at;
        let period_seconds = policy.period_seconds;
        let period_expired =
            now.saturating_sub(period_started_at) >= period_seconds.saturating_mul(1_000);
        let effective_period_jobs = if period_expired { 0 } else { usage.period_jobs };

        // 4. Validate quota.
        let n = batch.total_jobs;
        if n > policy.max_batch_jobs {
            return Err(StoreError::QuotaExceeded("batch_limit".into()));
        }
        if usage.active_jobs + n > policy.max_concurrent_jobs {
            return Err(StoreError::QuotaExceeded("inflight_limit".into()));
        }
        if effective_period_jobs + n > policy.max_jobs_per_period {
            return Err(StoreError::QuotaExceeded("period_limit".into()));
        }

        // 5. Validate shared input artifacts (same org, ready, not deleted).
        if !shared_input_artifact_ids.is_empty() {
            for artifact_id in shared_input_artifact_ids {
                let artifact: Option<(String,)> = sqlx::query_as(
                    "SELECT id FROM artifacts WHERE organization_id = $1 AND id = $2 AND state = \
                     'ready'",
                )
                .bind(batch.organization_id)
                .bind(artifact_id)
                .fetch_optional(&mut *tx)
                .await?;
                if artifact.is_none() {
                    return Err(StoreError::Conflict(format!(
                        "shared input artifact {artifact_id} is not ready or not found"
                    )));
                }
            }
        }

        // 6. Insert batch.
        sqlx::query(
            "INSERT INTO job_batches (id, organization_id, actor_id, actor_kind, actor_user_id, \
             workflow_id, workflow_version, workflow_content_digest, base_parameters_json, \
             variation_spec_json, device_organization_id, device_id, total_jobs, \
             retry_of_batch_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, \
             $9, $10, $11, $12, $13, $14, $15, $16)",
        )
        .bind(batch.batch_id)
        .bind(batch.organization_id)
        .bind(batch.actor_id)
        .bind(batch.actor_kind)
        .bind(batch.actor_user_id)
        .bind(batch.workflow_id)
        .bind(batch.workflow_version)
        .bind(batch.workflow_content_digest)
        .bind(batch.base_parameters_json)
        .bind(batch.variation_spec_json)
        .bind(batch.device_organization_id)
        .bind(batch.device_id)
        .bind(batch.total_jobs)
        .bind(batch.retry_of_batch_id)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // 7. Batch insert child jobs + input links + dispatch queue.
        for child in children {
            let input_ids_json =
                serde_json::to_string(child.input_artifact_ids).unwrap_or_else(|_| "[]".into());
            sqlx::query(
                "INSERT INTO jobs (organization_id, id, actor_id, actor_kind, actor_user_id, \
                 workflow_id, workflow_version, parameters_json, input_artifact_ids_json, \
                 output_artifact_ids_json, attempt, state, last_event, created_at, updated_at, \
                 batch_id, batch_index, client_item_id, queued_at) VALUES ($1, $2, $3, $4, $5, \
                 $6, $7, $8, $9, '[]', 1, 'queued', 0, $10, $10, $11, $12, $13, $10)",
            )
            .bind(batch.organization_id)
            .bind(child.job_id)
            .bind(batch.actor_id)
            .bind(batch.actor_kind)
            .bind(batch.actor_user_id)
            .bind(batch.workflow_id)
            .bind(batch.workflow_version)
            .bind(child.parameters_json)
            .bind(&input_ids_json)
            .bind(now)
            .bind(batch.batch_id)
            .bind(child.batch_index)
            .bind(child.client_item_id)
            .execute(&mut *tx)
            .await?;

            // Link shared input artifacts.
            for (index, artifact_id) in child.input_artifact_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO job_input_artifacts (organization_id, job_id, artifact_id, \
                     input_index) VALUES ($1, $2, $3, $4)",
                )
                .bind(batch.organization_id)
                .bind(child.job_id)
                .bind(artifact_id)
                .bind(index as i64)
                .execute(&mut *tx)
                .await?;
            }

            // Add to dispatch queue.
            sqlx::query(
                "INSERT INTO job_dispatch_queue (organization_id, job_id, available_at, priority, \
                 created_at) VALUES ($1, $2, $3, 0, $3)",
            )
            .bind(batch.organization_id)
            .bind(child.job_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }

        // 8. Update quota.
        sqlx::query(
            "UPDATE quota_usage SET active_jobs = active_jobs + $1, period_jobs = $2, \
             period_started_at = $3, updated_at = $4 WHERE organization_id = $5",
        )
        .bind(n)
        .bind(effective_period_jobs + n)
        .bind(if period_expired {
            now
        } else {
            period_started_at
        })
        .bind(now)
        .bind(batch.organization_id)
        .execute(&mut *tx)
        .await?;

        // 9. Insert batch idempotency record.
        if let Some(idem) = idempotency {
            sqlx::query(
                "INSERT INTO job_batch_idempotency_records (organization_id, actor_kind, \
                 actor_id, endpoint, idempotency_key, request_hash, batch_id, created_at) VALUES \
                 ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(idem.organization_id)
            .bind(idem.actor_kind)
            .bind(idem.actor_id)
            .bind(idem.endpoint)
            .bind(idem.key)
            .bind(idem.request_hash)
            .bind(idem.batch_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(CommitBatchResult::Created)
    }

    /// Retrieves a batch by ID.
    pub async fn job_batch(
        &self,
        organization_id: &str,
        batch_id: &str,
    ) -> Result<Option<StoredJobBatch>, StoreError> {
        Ok(query_as::<_, JobBatchRow>(
            "SELECT * FROM job_batches WHERE organization_id = $1 AND id = $2",
        )
        .bind(organization_id)
        .bind(batch_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    /// Lists batches for an organization, newest first, keyset paginated.
    pub async fn job_batches_page(
        &self,
        organization_id: &str,
        limit: i64,
        after: Option<(i64, &str)>,
    ) -> Result<Vec<StoredJobBatch>, StoreError> {
        let limit = limit.max(1);
        let rows = match after {
            None => {
                query_as::<_, JobBatchRow>(
                    "SELECT * FROM job_batches WHERE organization_id = $1 ORDER BY created_at \
                     DESC, id DESC LIMIT $2",
                )
                .bind(organization_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some((created_at, id)) => {
                query_as::<_, JobBatchRow>(
                    "SELECT * FROM job_batches WHERE organization_id = $1 AND (created_at, id) < \
                     ($2, $3) ORDER BY created_at DESC, id DESC LIMIT $4",
                )
                .bind(organization_id)
                .bind(created_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Counts jobs in each state for a batch.
    pub async fn batch_job_counts(
        &self,
        organization_id: &str,
        batch_id: &str,
    ) -> Result<BatchJobCounts, StoreError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT state, COUNT(*) FROM jobs WHERE organization_id = $1 AND batch_id = $2 GROUP \
             BY state",
        )
        .bind(organization_id)
        .bind(batch_id)
        .fetch_all(&self.pool)
        .await?;
        let mut counts = BatchJobCounts::default();
        for (state, count) in rows {
            match state.as_str() {
                "queued" => counts.queued = count,
                "received" => counts.received = count,
                "accepted" => counts.accepted = count,
                "running" => counts.running = count,
                "uploading" => counts.uploading = count,
                "completed" => counts.completed = count,
                "failed" => counts.failed = count,
                "cancelled" => counts.cancelled = count,
                _ => {}
            }
        }
        Ok(counts)
    }

    /// Lists child jobs of a batch, keyset paginated by batch_index.
    pub async fn batch_jobs_page(
        &self,
        organization_id: &str,
        batch_id: &str,
        limit: i64,
        after_index: Option<i64>,
    ) -> Result<Vec<StoredJobSummary>, StoreError> {
        let limit = limit.max(1);
        let rows = match after_index {
            None => {
                query_as::<_, JobSummaryRow>(
                    "SELECT id, batch_id, batch_index, state, progress, workflow_id, \
                     workflow_version, created_at, updated_at FROM jobs WHERE organization_id = \
                     $1 AND batch_id = $2 ORDER BY batch_index ASC LIMIT $3",
                )
                .bind(organization_id)
                .bind(batch_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some(idx) => {
                query_as::<_, JobSummaryRow>(
                    "SELECT id, batch_id, batch_index, state, progress, workflow_id, \
                     workflow_version, created_at, updated_at FROM jobs WHERE organization_id = \
                     $1 AND batch_id = $2 AND batch_index > $3 ORDER BY batch_index ASC LIMIT $4",
                )
                .bind(organization_id)
                .bind(batch_id)
                .bind(idx)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Claims the next dispatchable job globally.
    ///
    /// The queue row belongs to the consumer organization, while a batch may
    /// target a shared device in another organization. Scanning by connected
    /// worker organization therefore strands shared-device jobs whose consumer
    /// owns no Worker. The scheduler claims globally, then uses the batch's
    /// durable target to select the exact session.
    pub async fn claim_dispatch_queue_job(
        &self,
        lease_owner: &str,
        lease_seconds: i64,
        now: i64,
    ) -> Result<Option<(String, String)>, StoreError> {
        // Queue timestamps are Unix milliseconds. Keep the public duration in
        // seconds to match the scheduler configuration, but convert it before
        // comparing it with `available_at`/`lease_until`.
        let lease_until = now.saturating_add(lease_seconds.max(0).saturating_mul(1_000));
        let row: Option<(String, String)> = sqlx::query_as(
            "WITH candidate AS (SELECT organization_id,job_id FROM job_dispatch_queue WHERE \
             available_at <= $1 AND (lease_until IS NULL OR lease_until < $1) ORDER BY \
             available_at ASC,priority DESC,created_at ASC,organization_id,job_id LIMIT 1 FOR \
             UPDATE SKIP LOCKED) UPDATE job_dispatch_queue q SET lease_owner=$2,lease_until=$3 \
             FROM candidate c WHERE q.organization_id=c.organization_id AND q.job_id=c.job_id \
             RETURNING q.organization_id,q.job_id",
        )
        .bind(now)
        .bind(lease_owner)
        .bind(lease_until)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Removes a job from the dispatch queue (after it has been dispatched).
    pub async fn remove_from_dispatch_queue(
        &self,
        organization_id: &str,
        job_id: &str,
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM job_dispatch_queue WHERE organization_id = $1 AND job_id = $2")
            .bind(organization_id)
            .bind(job_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Counts dispatchable jobs in the backlog for an organization.
    pub async fn dispatch_queue_depth(&self, organization_id: &str) -> Result<i64, StoreError> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM job_dispatch_queue WHERE organization_id = $1")
                .bind(organization_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }

    /// Binds a queued job to a Worker, creates its durable outbox row, and
    /// removes it from the backlog as one transaction.
    ///
    /// Returning `false` means another scheduler (or cancellation) won the
    /// conditional `queued` update. In that case no outbox row is inserted and
    /// the caller must release its in-memory capacity reservation.
    pub async fn bind_queued_job(
        &self,
        organization_id: &str,
        job_id: &str,
        worker_organization_id: &str,
        worker_id: &str,
        session_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE jobs SET \
             worker_id=$4,worker_organization_id=$3,session_id=$5,state='received',updated_at=$6 \
             WHERE organization_id=$1 AND id=$2 AND state='queued'",
        )
        .bind(organization_id)
        .bind(job_id)
        .bind(worker_organization_id)
        .bind(worker_id)
        .bind(session_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO dispatch_outbox (organization_id, job_id, attempt, status, available_at) \
             SELECT $1, $2, attempt, 'pending', $3 FROM jobs WHERE organization_id = $1 AND id = \
             $2 ON CONFLICT (organization_id, job_id, attempt) DO NOTHING",
        )
        .bind(organization_id)
        .bind(job_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM job_dispatch_queue WHERE organization_id = $1 AND job_id = $2")
            .bind(organization_id)
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Cancels one unbound queued job and releases its concurrency reservation
    /// exactly once. The backlog row and job state change share a transaction,
    /// so the scheduler cannot dispatch a job after cancellation succeeds.
    pub async fn cancel_queued_job(
        &self,
        organization_id: &str,
        job_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE jobs SET state='cancelled',error='cancelled before worker dispatch', \
             updated_at=$1 WHERE organization_id=$2 AND id=$3 AND state='queued'",
        )
        .bind(now)
        .bind(organization_id)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM job_dispatch_queue WHERE organization_id=$1 AND job_id=$2")
            .bind(organization_id)
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        crate::quota::release_job_for_terminal_tx(&mut tx, organization_id, job_id, now).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Returns `(workflow_id, workflow_version, device_organization_id,
    /// device_id, attempt)` for a queued batch child.
    pub async fn queued_job_info(
        &self,
        organization_id: &str,
        job_id: &str,
    ) -> Result<Option<(String, String, String, String, i64)>, StoreError> {
        let row = sqlx::query_as(
            "SELECT j.workflow_id,j.workflow_version,b.device_organization_id,b.device_id,j.\
             attempt FROM jobs j JOIN job_batches b ON b.organization_id=j.organization_id AND \
             b.id=j.batch_id WHERE j.organization_id=$1 AND j.id=$2 AND j.state='queued' AND \
             b.device_organization_id IS NOT NULL AND b.device_id IS NOT NULL",
        )
        .bind(organization_id)
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }
}
