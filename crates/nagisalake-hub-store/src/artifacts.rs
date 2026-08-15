use crate::{rows::*, *};
use sqlx::{query, query_as};

impl PgStore {
    pub async fn create_artifact(&self, input: ArtifactUpsert<'_>) -> Result<(), StoreError> {
        query(
            "INSERT INTO artifacts \
             (organization_id,id,job_id,name,content_type,size_bytes,sha256,state,object_key,\
             created_at,updated_at,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$10,$11) ON \
             CONFLICT (organization_id,id) DO UPDATE SET \
             job_id=EXCLUDED.job_id,name=EXCLUDED.name,content_type=EXCLUDED.content_type,\
             size_bytes=EXCLUDED.size_bytes,sha256=EXCLUDED.sha256,state=EXCLUDED.state,\
             object_key=EXCLUDED.object_key,updated_at=EXCLUDED.updated_at,expires_at=EXCLUDED.\
             expires_at",
        )
        .bind(input.organization_id)
        .bind(input.id)
        .bind(input.job_id)
        .bind(input.name)
        .bind(input.content_type)
        .bind(input.size_bytes as i64)
        .bind(input.sha256)
        .bind(input.state)
        .bind(input.object_key)
        .bind(input.now)
        .bind(input.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Deletes pending uploads whose deadline passed and releases their quota in
    /// the same transaction.
    ///
    /// Metadata and quota must move together: releasing quota without deleting
    /// the row would let the same artifact be reclaimed twice and drive usage
    /// below reality, while deleting the row without releasing quota strands the
    /// reservation permanently. The returned rows still have objects to delete,
    /// which the caller does afterwards — a leftover object in storage is
    /// recoverable, a wrong quota is not.
    ///
    /// `DELETE ... RETURNING` with `FOR UPDATE SKIP LOCKED` semantics via the
    /// subquery keeps two concurrent reapers from double-counting.
    pub async fn reclaim_expired_uploads(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<ReclaimedUpload>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let expired = query_as::<_, (String, String, String, i64)>(
            "DELETE FROM artifacts WHERE (organization_id,id) IN (SELECT organization_id,id FROM \
             artifacts WHERE state='pending_upload' AND expires_at IS NOT NULL AND expires_at <= \
             $1 ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED) RETURNING \
             organization_id,id,object_key,size_bytes",
        )
        .bind(now)
        .bind(limit.max(1))
        .fetch_all(&mut *tx)
        .await?;

        for (organization_id, _id, _object_key, size_bytes) in &expired {
            query(
                "UPDATE quota_usage SET storage_bytes=GREATEST(0,storage_bytes-$1),updated_at=$2 \
                 WHERE organization_id=$3",
            )
            .bind((*size_bytes).max(0))
            .bind(now)
            .bind(organization_id)
            .execute(&mut *tx)
            .await?;
        }

        // Orphaned upload request rows would otherwise outlive their artifact.
        for (organization_id, id, _object_key, _size_bytes) in &expired {
            query(
                "DELETE FROM artifact_upload_requests WHERE organization_id=$1 AND artifact_id=$2",
            )
            .bind(organization_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(expired
            .into_iter()
            .map(
                |(organization_id, id, object_key, size_bytes)| ReclaimedUpload {
                    organization_id,
                    id,
                    object_key,
                    size_bytes,
                },
            )
            .collect())
    }

    pub async fn artifact(
        &self,
        organization_id: &str,
        id: &str,
    ) -> Result<Option<StoredArtifact>, StoreError> {
        Ok(query_as::<_, ArtifactRow>(
            "SELECT organization_id,id,job_id,name,content_type,size_bytes,sha256,state,\
             object_key,created_at,updated_at FROM artifacts WHERE organization_id=$1 AND id=$2",
        )
        .bind(organization_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn artifacts(
        &self,
        organization_id: &str,
    ) -> Result<Vec<StoredArtifact>, StoreError> {
        Ok(query_as::<_, ArtifactRow>(
            "SELECT organization_id,id,job_id,name,content_type,size_bytes,sha256,state,\
             object_key,created_at,updated_at FROM artifacts WHERE organization_id=$1 ORDER BY \
             created_at",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn organization_object_keys(
        &self,
        organization_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        Ok(
            query_as::<_, (String,)>("SELECT object_key FROM artifacts WHERE organization_id=$1")
                .bind(organization_id)
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .map(|(key,)| key)
                .collect(),
        )
    }

    pub async fn all_artifacts(&self) -> Result<Vec<StoredArtifact>, StoreError> {
        Ok(query_as::<_, ArtifactRow>(
            "SELECT organization_id,id,job_id,name,content_type,size_bytes,sha256,state,\
             object_key,created_at,updated_at FROM artifacts ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// Updates an artifact's state, clearing the pending-upload deadline once it
    /// is no longer pending.
    ///
    /// Leaving `expires_at` set on a `ready` artifact would let the reaper
    /// delete real data and release quota it still occupies.
    pub async fn set_artifact_state(
        &self,
        organization_id: &str,
        id: &str,
        state: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        Ok(query(
            "UPDATE artifacts SET state=$1,updated_at=$2,expires_at=CASE WHEN $1='pending_upload' \
             THEN expires_at ELSE NULL END WHERE organization_id=$3 AND id=$4",
        )
        .bind(state)
        .bind(now)
        .bind(organization_id)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn bind_artifact_job(
        &self,
        organization_id: &str,
        id: &str,
        job_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        Ok(query(
            "UPDATE artifacts SET job_id=$1,updated_at=$2 WHERE organization_id=$3 AND id=$4 AND \
             job_id IS NULL",
        )
        .bind(job_id)
        .bind(now)
        .bind(organization_id)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn upsert_upload_request(
        &self,
        input: UploadRequestUpsert<'_>,
    ) -> Result<(), StoreError> {
        query(
            "INSERT INTO artifact_upload_requests \
             (organization_id,request_id,artifact_id,job_id,attempt,created_at) VALUES \
             ($1,$2,$3,$4,$5,$6) ON CONFLICT (organization_id,request_id) DO NOTHING",
        )
        .bind(input.organization_id)
        .bind(input.request_id)
        .bind(input.artifact_id)
        .bind(input.job_id)
        .bind(input.attempt)
        .bind(input.now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upload_request_artifact(
        &self,
        organization_id: &str,
        request_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(query_as::<_, (String,)>(
            "SELECT artifact_id FROM artifact_upload_requests WHERE organization_id=$1 AND \
             request_id=$2",
        )
        .bind(organization_id)
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await?
        .map(|row| row.0))
    }

    pub async fn complete_job_output_upload(
        &self,
        input: CompleteJobOutputUpload<'_>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let artifact = query_as::<_, (Option<String>, String)>(
            "SELECT job_id,state FROM artifacts WHERE organization_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(input.organization_id)
        .bind(input.artifact_id)
        .fetch_optional(&mut *tx)
        .await?;
        if artifact.as_ref().is_none_or(|(job_id, state)| {
            job_id.as_deref() != Some(input.job_id)
                || !matches!(state.as_str(), "pending_upload" | "ready")
        }) {
            tx.rollback().await?;
            return Ok(false);
        }
        let request = query_as::<_, (String, Option<String>, Option<i64>)>(
            "SELECT artifact_id,job_id,attempt FROM artifact_upload_requests WHERE \
             organization_id=$1 AND request_id=$2 FOR UPDATE",
        )
        .bind(input.organization_id)
        .bind(input.request_id)
        .fetch_optional(&mut *tx)
        .await?;
        if request
            .as_ref()
            .is_none_or(|(artifact_id, job_id, attempt)| {
                artifact_id != input.artifact_id
                    || job_id.as_deref() != Some(input.job_id)
                    || *attempt != Some(input.attempt)
            })
        {
            tx.rollback().await?;
            return Ok(false);
        }
        let job = query(
            "UPDATE jobs SET output_artifact_ids_json=CASE WHEN output_artifact_ids_json::jsonb ? \
             $1 THEN output_artifact_ids_json ELSE (output_artifact_ids_json::jsonb || \
             jsonb_build_array($1::text))::text END,updated_at=GREATEST(updated_at,$2) WHERE \
             organization_id=$3 AND id=$4 AND attempt=$5 AND session_id=$6",
        )
        .bind(input.artifact_id)
        .bind(input.now)
        .bind(input.organization_id)
        .bind(input.job_id)
        .bind(input.attempt)
        .bind(input.session_id)
        .execute(&mut *tx)
        .await?;
        if job.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        query(
            "UPDATE artifacts SET \
             state='ready',updated_at=GREATEST(updated_at,$1),expires_at=NULL WHERE \
             organization_id=$2 AND id=$3",
        )
        .bind(input.now)
        .bind(input.organization_id)
        .bind(input.artifact_id)
        .execute(&mut *tx)
        .await?;
        query(
            "UPDATE artifact_upload_requests SET completed_at=COALESCE(completed_at,$1) WHERE \
             organization_id=$2 AND request_id=$3",
        )
        .bind(input.now)
        .bind(input.organization_id)
        .bind(input.request_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn all_upload_requests(&self) -> Result<Vec<StoredUploadRequest>, StoreError> {
        Ok(query_as::<_, UploadRequestRow>(
            "SELECT organization_id,request_id,artifact_id,job_id,attempt,created_at,completed_at \
             FROM artifact_upload_requests",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }
}
