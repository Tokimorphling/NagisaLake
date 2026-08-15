use crate::{rows::*, *};
use sqlx::{AssertSqlSafe, query, query_as};

impl PgStore {
    pub async fn upsert_workflow(&self, input: WorkflowUpsert<'_>) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        query(
            "INSERT INTO workflow_versions \
             (organization_id,workflow_id,version,manifest_json,output_types_json,content_hash,\
             updated_at,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$7) ON CONFLICT \
             (organization_id,workflow_id,version) DO UPDATE SET \
             manifest_json=EXCLUDED.manifest_json,output_types_json=EXCLUDED.output_types_json,\
             approval_state=CASE WHEN workflow_versions.content_hash IS DISTINCT FROM \
             EXCLUDED.content_hash THEN 'drifted' ELSE workflow_versions.approval_state \
             END,content_hash=EXCLUDED.content_hash,updated_at=EXCLUDED.updated_at",
        )
        .bind(input.organization_id)
        .bind(input.workflow_id)
        .bind(input.version)
        .bind(input.manifest_json)
        .bind(input.output_types_json)
        .bind(input.content_hash)
        .bind(input.now)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO worker_workflows \
             (organization_id,worker_id,workflow_id,version,last_seen_at) VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (organization_id,worker_id,workflow_id,version) DO UPDATE SET \
             last_seen_at=EXCLUDED.last_seen_at",
        )
        .bind(input.organization_id)
        .bind(input.worker_id)
        .bind(input.workflow_id)
        .bind(input.version)
        .bind(input.now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Drops this worker's links to workflow versions it no longer offers.
    ///
    /// `upsert_workflow` only inserts and updates, so a version the worker stops
    /// reporting keeps its `worker_workflows` row forever. The catalog joins
    /// through that table, so the stale version stays listed with no online
    /// device and no way to remove it — renaming `v1` to `v2` in a worker config
    /// leaves `v1` visible for good.
    ///
    /// Only the link is removed. The `workflow_versions` row survives because
    /// historical jobs reference `(workflow_id, version)`, and another worker may
    /// still offer the same version. A version with no remaining link simply
    /// stops appearing in the catalog.
    ///
    /// Passing an empty `keep` removes every link for the worker, which is what
    /// a worker that registered with no workflows should mean.
    pub async fn retain_worker_workflows(
        &self,
        organization_id: &str,
        worker_id: &str,
        keep: &[(String, String)],
    ) -> Result<u64, StoreError> {
        let workflow_ids: Vec<String> = keep.iter().map(|(id, _)| id.clone()).collect();
        let versions: Vec<String> = keep.iter().map(|(_, version)| version.clone()).collect();
        let removed = query(
            "DELETE FROM worker_workflows WHERE organization_id=$1 AND worker_id=$2 AND \
             (workflow_id, version) NOT IN (SELECT * FROM unnest($3::text[], $4::text[]))",
        )
        .bind(organization_id)
        .bind(worker_id)
        .bind(&workflow_ids)
        .bind(&versions)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(removed)
    }

    pub async fn workflows_for_org(
        &self,
        organization_id: &str,
    ) -> Result<Vec<StoredWorkflow>, StoreError> {
        Ok(query_as::<_, WorkflowRow>(
            "SELECT organization_id,workflow_id,version,manifest_json,output_types_json,\
             content_hash FROM workflow_versions WHERE organization_id=$1 ORDER BY \
             workflow_id,version",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn workflows_for_user_devices(
        &self,
        user_id: &str,
        organization_id: &str,
    ) -> Result<Vec<StoredWorkflow>, StoreError> {
        Ok(query_as::<_, WorkflowRow>(
            "SELECT DISTINCT \
             v.organization_id,v.workflow_id,v.version,v.manifest_json,v.output_types_json,v.\
             content_hash FROM workflow_versions v JOIN worker_workflows x ON \
             x.organization_id=v.organization_id AND x.workflow_id=v.workflow_id AND \
             x.version=v.version JOIN workers w ON w.organization_id=x.organization_id AND \
             w.id=x.worker_id LEFT JOIN device_grants g ON \
             g.device_organization_id=w.organization_id AND g.device_id=w.id AND \
             g.grantee_user_id=$1 AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR \
             g.expires_at>$3) WHERE v.approval_state='approved' AND (w.organization_id=$2 OR \
             (g.id IS NOT NULL AND (g.allowed_workflows_json::jsonb='[]'::jsonb OR EXISTS (SELECT \
             1 FROM jsonb_to_recordset(g.allowed_workflows_json::jsonb) AS rule(id text, version \
             text) WHERE rule.id=v.workflow_id AND rule.version=v.version)))) ORDER BY \
             v.workflow_id,v.version",
        )
        .bind(user_id)
        .bind(organization_id)
        .bind(now_unix_ms())
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// One page of the distinct public workflow catalog visible to a user.
    /// Live worker state is merged by the Hub after this durable page is read.
    pub async fn workflows_for_user_devices_page(
        &self,
        user_id: &str,
        organization_id: &str,
        limit: i64,
        after: Option<(&str, &str)>,
    ) -> Result<Vec<StoredWorkflow>, StoreError> {
        let now = now_unix_ms();
        const SELECT: &str =
            "SELECT DISTINCT ON (v.workflow_id,v.version) \
             v.organization_id,v.workflow_id,v.version,v.manifest_json,v.output_types_json,v.\
             content_hash FROM workflow_versions v JOIN worker_workflows x ON \
             x.organization_id=v.organization_id AND x.workflow_id=v.workflow_id AND \
             x.version=v.version JOIN workers w ON w.organization_id=x.organization_id AND \
             w.id=x.worker_id LEFT JOIN device_grants g ON \
             g.device_organization_id=w.organization_id AND g.device_id=w.id AND \
             g.grantee_user_id=$1 AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR \
             g.expires_at>$3) WHERE v.approval_state='approved' AND (w.organization_id=$2 OR \
             (g.id IS NOT NULL AND (g.allowed_workflows_json::jsonb='[]'::jsonb OR EXISTS (SELECT \
             1 FROM jsonb_to_recordset(g.allowed_workflows_json::jsonb) AS rule(id text, version \
             text) WHERE rule.id=v.workflow_id AND rule.version=v.version))))";
        let limit = limit.max(1);
        let rows = match after {
            None => {
                query_as::<_, WorkflowRow>(AssertSqlSafe(format!(
                    "{SELECT} ORDER BY v.workflow_id,v.version,v.organization_id LIMIT $4"
                )))
                .bind(user_id)
                .bind(organization_id)
                .bind(now)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some((workflow_id, version)) => {
                query_as::<_, WorkflowRow>(AssertSqlSafe(format!(
                    "{SELECT} AND (v.workflow_id,v.version) > ($4,$5) ORDER BY \
                     v.workflow_id,v.version,v.organization_id LIMIT $6"
                )))
                .bind(user_id)
                .bind(organization_id)
                .bind(now)
                .bind(workflow_id)
                .bind(version)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }
}
