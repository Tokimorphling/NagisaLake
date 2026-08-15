use crate::{rows::*, *};
use sqlx::{AssertSqlSafe, query, query_as};
use uuid::Uuid;

impl PgStore {
    pub async fn audit(&self, entry: AuditInsert<'_>) -> Result<(), StoreError> {
        query(
            "INSERT INTO audit_logs \
             (id,organization_id,actor_id,actor_kind,request_id,action,resource_type,resource_id,\
             outcome,metadata_json,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(entry.organization_id)
        .bind(entry.actor_id)
        .bind(entry.actor_kind)
        .bind(entry.request_id)
        .bind(entry.action)
        .bind(entry.resource_type)
        .bind(entry.resource_id)
        .bind(entry.outcome)
        .bind(entry.metadata_json)
        .bind(entry.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn audit_logs(
        &self,
        organization_id: &str,
        limit: i64,
    ) -> Result<Vec<AuditLog>, StoreError> {
        Ok(query_as::<_, AuditRow>(
            "SELECT id,organization_id,actor_id,actor_kind,request_id,action,resource_type,\
             resource_id,outcome,metadata_json,created_at FROM audit_logs WHERE \
             organization_id=$1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(organization_id)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// One keyset page of audit records, newest first. The id tie-breaker is
    /// required because multiple actions can share the same millisecond.
    pub async fn audit_logs_page(
        &self,
        organization_id: &str,
        limit: i64,
        after: Option<(i64, &str)>,
    ) -> Result<Vec<AuditLog>, StoreError> {
        const COLUMNS: &str = "id,organization_id,actor_id,actor_kind,request_id,action,\
                               resource_type,resource_id,outcome,metadata_json,created_at";
        let limit = limit.max(1);
        let rows = match after {
            None => {
                query_as::<_, AuditRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM audit_logs WHERE organization_id=$1 ORDER BY \
                     created_at DESC,id DESC LIMIT $2"
                )))
                .bind(organization_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some((created_at, id)) => {
                query_as::<_, AuditRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM audit_logs WHERE organization_id=$1 AND \
                     (created_at,id) < ($2,$3) ORDER BY created_at DESC,id DESC LIMIT $4"
                )))
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

    pub async fn audit_logs_all(&self, organization_id: &str) -> Result<Vec<AuditLog>, StoreError> {
        Ok(query_as::<_, AuditRow>(
            "SELECT id,organization_id,actor_id,actor_kind,request_id,action,resource_type,\
             resource_id,outcome,metadata_json,created_at FROM audit_logs WHERE \
             organization_id=$1 ORDER BY created_at DESC",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }
}
