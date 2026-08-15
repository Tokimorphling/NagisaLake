use crate::{rows::*, *};
use sqlx::{Postgres, Transaction, query, query_as};
use uuid::Uuid;

pub(crate) async fn release_job_for_terminal_tx(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: &str,
    job_id: &str,
    now: i64,
) -> Result<(), StoreError> {
    let result = query(
        "INSERT INTO usage_ledger \
         (id,organization_id,job_id,metric,amount,idempotency_key,created_at) SELECT \
         $1,$2,$3,'concurrency_release',1,$3,$4 FROM jobs WHERE organization_id=$2 AND id=$3 AND \
         state IN ('completed','failed','cancelled') ON CONFLICT \
         (organization_id,metric,idempotency_key) DO NOTHING",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(organization_id)
    .bind(job_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 1 {
        query(
            "UPDATE quota_usage SET active_jobs=GREATEST(0,active_jobs-1),updated_at=$1 WHERE \
             organization_id=$2",
        )
        .bind(now)
        .bind(organization_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

impl PgStore {
    pub async fn reserve_storage(
        &self,
        organization_id: &str,
        bytes: i64,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let quota = query_as::<_, (i64, i64)>(
            "SELECT max_storage_bytes, period_seconds FROM quota_policies WHERE \
             organization_id=$1 FOR SHARE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("quota policy".into()))?;
        let usage = query_as::<_, (i64,)>(
            "SELECT storage_bytes FROM quota_usage WHERE organization_id=$1 FOR UPDATE",
        )
        .bind(organization_id)
        .fetch_one(&mut *tx)
        .await?;
        if bytes < 0 || usage.0.saturating_add(bytes) > quota.0 {
            return Err(StoreError::QuotaExceeded("storage_bytes".into()));
        }
        query(
            "UPDATE quota_usage SET storage_bytes=storage_bytes+$1,updated_at=$2 WHERE \
             organization_id=$3",
        )
        .bind(bytes)
        .bind(now_unix_ms())
        .bind(organization_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn release_storage(
        &self,
        organization_id: &str,
        bytes: i64,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE quota_usage SET storage_bytes=GREATEST(0,storage_bytes-$1),updated_at=$2 \
             WHERE organization_id=$3",
        )
        .bind(bytes.max(0))
        .bind(now_unix_ms())
        .bind(organization_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn reserve_job(&self, organization_id: &str) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let quota = query_as::<_, (i64, i64, i64, i64)>(
            "SELECT max_concurrent_jobs,max_jobs_per_period,period_seconds,updated_at FROM \
             quota_policies WHERE organization_id=$1 FOR SHARE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("quota policy".into()))?;
        let usage = query_as::<_, (i64, i64, i64)>(
            "SELECT active_jobs,period_jobs,period_started_at FROM quota_usage WHERE \
             organization_id=$1 FOR UPDATE",
        )
        .bind(organization_id)
        .fetch_one(&mut *tx)
        .await?;
        let now = now_unix_ms();
        let period_jobs = if now.saturating_sub(usage.2) >= quota.2.saturating_mul(1_000) {
            0
        } else {
            usage.1
        };
        if usage.0 >= quota.0 || period_jobs >= quota.1 {
            return Err(StoreError::QuotaExceeded("jobs".into()));
        }
        query(
            "UPDATE quota_usage SET \
             active_jobs=active_jobs+1,period_jobs=$1,period_started_at=CASE WHEN $2=0 THEN $3 \
             ELSE period_started_at END,updated_at=$3 WHERE organization_id=$4",
        )
        .bind(period_jobs + 1)
        .bind(period_jobs)
        .bind(now)
        .bind(organization_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn release_job(&self, organization_id: &str) -> Result<(), StoreError> {
        query(
            "UPDATE quota_usage SET active_jobs=GREATEST(0,active_jobs-1),updated_at=$1 WHERE \
             organization_id=$2",
        )
        .bind(now_unix_ms())
        .bind(organization_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Releases concurrency exactly once for a terminal job, even if the
    /// worker repeats its at-least-once terminal event.
    pub async fn release_job_for_terminal(
        &self,
        organization_id: &str,
        job_id: &str,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        release_job_for_terminal_tx(&mut tx, organization_id, job_id, now_unix_ms()).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn quota(&self, organization_id: &str) -> Result<QuotaSnapshot, StoreError> {
        let row = query_as::<_, QuotaRow>(
            "SELECT p.organization_id,p.max_concurrent_jobs,p.max_storage_bytes,p.\
             max_jobs_per_period,p.period_seconds,u.active_jobs,u.storage_bytes,u.period_jobs,u.\
             period_started_at FROM quota_policies p JOIN quota_usage u ON \
             u.organization_id=p.organization_id WHERE p.organization_id=$1",
        )
        .bind(organization_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound("quota".into()))?;
        Ok(row.into())
    }

    pub async fn update_quota_policy(
        &self,
        input: QuotaPolicyUpdate<'_>,
    ) -> Result<QuotaSnapshot, StoreError> {
        if input.max_concurrent_jobs <= 0
            || input.max_storage_bytes <= 0
            || input.max_jobs_per_period <= 0
            || input.period_seconds <= 0
        {
            return Err(StoreError::InvalidConfig(
                "quota policy values must be greater than zero".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        let exists = query_as::<_, (String,)>(
            "SELECT organization_id FROM quota_policies WHERE organization_id=$1 FOR UPDATE",
        )
        .bind(input.organization_id)
        .fetch_optional(&mut *tx)
        .await?;
        if exists.is_none() {
            return Err(StoreError::NotFound("quota policy".into()));
        }
        query(
            "UPDATE quota_policies SET \
             max_concurrent_jobs=$1,max_storage_bytes=$2,max_jobs_per_period=$3,period_seconds=$4,\
             updated_at=$5 WHERE organization_id=$6",
        )
        .bind(input.max_concurrent_jobs)
        .bind(input.max_storage_bytes)
        .bind(input.max_jobs_per_period)
        .bind(input.period_seconds)
        .bind(now_unix_ms())
        .bind(input.organization_id)
        .execute(&mut *tx)
        .await?;
        let row = query_as::<_, QuotaRow>(
            "SELECT p.organization_id,p.max_concurrent_jobs,p.max_storage_bytes,p.\
             max_jobs_per_period,p.period_seconds,u.active_jobs,u.storage_bytes,u.period_jobs,u.\
             period_started_at FROM quota_policies p JOIN quota_usage u ON \
             u.organization_id=p.organization_id WHERE p.organization_id=$1",
        )
        .bind(input.organization_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.into())
    }

    /// Repairs active job quota from the jobs table and fails jobs whose worker
    /// has disappeared for longer than the reconciliation grace period.
    pub async fn reconcile_active_job_quota(
        &self,
        now: i64,
        stale_after_ms: i64,
    ) -> Result<QuotaReconcileStats, StoreError> {
        let cutoff = now.saturating_sub(stale_after_ms.max(1_000));
        let mut tx = self.pool.begin().await?;
        let stale_jobs = query_as::<_, (String, String)>(
            "UPDATE jobs j SET state='failed',error='worker session unavailable during quota \
             reconciliation',updated_at=$1 WHERE j.state NOT IN \
             ('completed','failed','cancelled') AND j.updated_at <= $2 AND NOT EXISTS (SELECT 1 \
             FROM workers w WHERE w.organization_id=j.worker_organization_id AND w.id=j.worker_id \
             AND w.last_seen_at > $2) RETURNING j.organization_id,j.id",
        )
        .bind(now)
        .bind(cutoff)
        .fetch_all(&mut *tx)
        .await?;
        for (organization_id, job_id) in &stale_jobs {
            query(
                "INSERT INTO usage_ledger \
                 (id,organization_id,job_id,metric,amount,idempotency_key,created_at) VALUES \
                 ($1,$2,$3,'concurrency_release',1,$3,$4) ON CONFLICT \
                 (organization_id,metric,idempotency_key) DO NOTHING",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(organization_id)
            .bind(job_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        let changed_nonzero = query(
            "WITH actual AS (SELECT organization_id,COUNT(*)::BIGINT AS active_jobs FROM jobs \
             WHERE state NOT IN ('completed','failed','cancelled') GROUP BY organization_id) \
             UPDATE quota_usage u SET active_jobs=a.active_jobs,updated_at=$1 FROM actual a WHERE \
             u.organization_id=a.organization_id AND u.active_jobs<>a.active_jobs",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let changed_zero = query(
            "UPDATE quota_usage u SET active_jobs=0,updated_at=$1 WHERE u.active_jobs<>0 AND NOT \
             EXISTS (SELECT 1 FROM jobs j WHERE j.organization_id=u.organization_id AND j.state \
             NOT IN ('completed','failed','cancelled'))",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let active_jobs =
            query_as::<_, (i64,)>("SELECT COALESCE(SUM(active_jobs),0)::BIGINT FROM quota_usage")
                .fetch_one(&mut *tx)
                .await?
                .0;
        tx.commit().await?;
        Ok(QuotaReconcileStats {
            corrected_organizations: changed_nonzero + changed_zero,
            failed_jobs: stale_jobs.len() as i64,
            active_jobs,
        })
    }
}
