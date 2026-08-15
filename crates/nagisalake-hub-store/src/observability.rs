use crate::{PgStore, StoreError};

/// Low-cardinality queue state sampled by the Hub outside the scrape path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BacklogMetrics {
    pub dispatch_queue_depth:               u64,
    pub dispatch_queue_oldest_ready_lag_ms: u64,
    pub outbox_pending_depth:               u64,
    pub outbox_claimed_depth:               u64,
    pub outbox_oldest_ready_lag_ms:         u64,
}

impl PgStore {
    /// Reads all scheduler/outbox gauges in one round trip. Counts are global,
    /// deliberately avoiding organization labels that would grow one series
    /// per tenant. Negative ages are clamped for clock-skew tolerance.
    pub async fn backlog_metrics(&self, now: i64) -> Result<BacklogMetrics, StoreError> {
        let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
            "SELECT (SELECT COUNT(*) FROM job_dispatch_queue), COALESCE((SELECT GREATEST($1 - \
             MIN(available_at), 0) FROM job_dispatch_queue WHERE available_at <= $1), 0), (SELECT \
             COUNT(*) FROM dispatch_outbox WHERE status = 'pending'), (SELECT COUNT(*) FROM \
             dispatch_outbox WHERE status = 'claimed'), COALESCE((SELECT GREATEST($1 - \
             MIN(available_at), 0) FROM dispatch_outbox WHERE status = 'pending' AND available_at \
             <= $1), 0)",
        )
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(BacklogMetrics {
            dispatch_queue_depth:               u64::try_from(row.0).unwrap_or_default(),
            dispatch_queue_oldest_ready_lag_ms: u64::try_from(row.1).unwrap_or_default(),
            outbox_pending_depth:               u64::try_from(row.2).unwrap_or_default(),
            outbox_claimed_depth:               u64::try_from(row.3).unwrap_or_default(),
            outbox_oldest_ready_lag_ms:         u64::try_from(row.4).unwrap_or_default(),
        })
    }
}
