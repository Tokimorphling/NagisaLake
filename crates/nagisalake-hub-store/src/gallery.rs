use crate::{rows::*, *};
use sqlx::{AssertSqlSafe, query_as};

const GALLERY_ITEM_SELECT: &str =
    "SELECT g.id,g.organization_id,g.artifact_id,g.job_id,g.owner_user_id,g.workflow_id,g.\
     workflow_version,g.display_name,g.parameters_json,g.published_at,a.name AS \
     artifact_name,a.content_type,a.size_bytes,a.sha256 FROM gallery_items g JOIN artifacts a ON \
     a.organization_id=g.organization_id AND a.id=g.artifact_id";

impl PgStore {
    /// Finds the immutable source data for a gallery publication.
    ///
    /// Ownership is the user that submitted the job, not the organization that
    /// owns the worker.  This distinction is essential for invited/shared
    /// devices: the output belongs to the consumer who requested it.
    pub async fn gallery_publish_candidate(
        &self,
        organization_id: &str,
        artifact_id: &str,
        owner_user_id: &str,
    ) -> Result<Option<GalleryPublishCandidate>, StoreError> {
        Ok(query_as::<_, GalleryPublishCandidateRow>(
            "SELECT a.id AS \
             artifact_id,a.content_type,j.workflow_id,j.workflow_version,j.parameters_json,v.\
             manifest_json FROM artifacts a JOIN jobs j ON j.organization_id=a.organization_id \
             AND j.id=a.job_id LEFT JOIN workflow_versions v ON \
             v.organization_id=j.worker_organization_id AND v.workflow_id=j.workflow_id AND \
             v.version=j.workflow_version WHERE a.organization_id=$1 AND a.id=$2 AND \
             a.state='ready' AND j.state='completed' AND j.actor_user_id=$3 AND \
             j.output_artifact_ids_json::jsonb ? a.id",
        )
        .bind(organization_id)
        .bind(artifact_id)
        .bind(owner_user_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    /// Publishes idempotently while rechecking the ownership and ready-output
    /// predicates in the inserting statement.  The second check prevents a
    /// future mutable artifact state from opening a check/use gap.
    pub async fn publish_gallery_item(
        &self,
        input: PublishGalleryItem<'_>,
    ) -> Result<StoredGalleryItem, StoreError> {
        let inserted = query_as::<_, (String,)>(
            "INSERT INTO gallery_items \
             (id,organization_id,artifact_id,job_id,owner_user_id,workflow_id,workflow_version,\
             display_name,parameters_json,published_at) SELECT \
             $1,a.organization_id,a.id,j.id,$4,j.workflow_id,j.workflow_version,$5,$6,$7 FROM \
             artifacts a JOIN jobs j ON j.organization_id=a.organization_id AND j.id=a.job_id \
             WHERE a.organization_id=$2 AND a.id=$3 AND a.state='ready' AND j.state='completed' \
             AND j.actor_user_id=$4 AND j.output_artifact_ids_json::jsonb ? a.id ON CONFLICT \
             (organization_id,artifact_id) DO UPDATE SET \
             owner_user_id=gallery_items.owner_user_id RETURNING gallery_items.id",
        )
        .bind(input.id)
        .bind(input.organization_id)
        .bind(input.artifact_id)
        .bind(input.owner_user_id)
        .bind(input.display_name)
        .bind(input.parameters_json)
        .bind(input.published_at)
        .fetch_optional(&self.pool)
        .await?;
        let Some((id,)) = inserted else {
            return Err(StoreError::Conflict(
                "artifact is not a ready completed output owned by the current user".into(),
            ));
        };
        self.gallery_item(&id)
            .await?
            .ok_or_else(|| StoreError::NotFound("gallery item".into()))
    }

    /// Lists gallery items newest-first across **all** organizations.
    ///
    /// Visibility is intentionally cross-organization: any authenticated user
    /// receives the same page. This is the "public gallery" semantic: only
    /// publication/unpublication are tenant-scoped (see [`publish_gallery_item`]
    /// and [`unpublish_gallery_item`]); reads are not.
    pub async fn gallery_items_page(
        &self,
        limit: i64,
        after: Option<(i64, &str)>,
    ) -> Result<Vec<StoredGalleryItem>, StoreError> {
        let limit = limit.max(1);
        let rows = match after {
            None => {
                query_as::<_, GalleryItemRow>(AssertSqlSafe(format!(
                    "{GALLERY_ITEM_SELECT} ORDER BY g.published_at DESC,g.id DESC LIMIT $1"
                )))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some((published_at, id)) => {
                query_as::<_, GalleryItemRow>(AssertSqlSafe(format!(
                    "{GALLERY_ITEM_SELECT} WHERE (g.published_at,g.id) < ($1,$2) ORDER BY \
                     g.published_at DESC,g.id DESC LIMIT $3"
                )))
                .bind(published_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn gallery_item(&self, id: &str) -> Result<Option<StoredGalleryItem>, StoreError> {
        Ok(query_as::<_, GalleryItemRow>(AssertSqlSafe(format!(
            "{GALLERY_ITEM_SELECT} WHERE g.id=$1"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn gallery_content(&self, id: &str) -> Result<Option<GalleryContent>, StoreError> {
        Ok(query_as::<_, GalleryContentRow>(
            "SELECT a.name AS artifact_name,a.content_type,a.size_bytes,a.sha256,a.object_key \
             FROM gallery_items g JOIN artifacts a ON a.organization_id=g.organization_id AND \
             a.id=g.artifact_id WHERE g.id=$1 AND a.state='ready'",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn unpublish_gallery_item(
        &self,
        id: &str,
        owner_user_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(query_as::<_, (String,)>(
            "DELETE FROM gallery_items WHERE id=$1 AND owner_user_id=$2 RETURNING organization_id",
        )
        .bind(id)
        .bind(owner_user_id)
        .fetch_optional(&self.pool)
        .await?
        .map(|(organization_id,)| organization_id))
    }
}
