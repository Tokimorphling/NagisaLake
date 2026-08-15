use crate::{rows::*, *};
use sqlx::{AssertSqlSafe, query, query_as};

impl PgStore {
    pub async fn create_api_key(&self, input: NewApiKey<'_>) -> Result<(), StoreError> {
        query(
            "INSERT INTO api_keys \
             (id,organization_id,creator_user_id,name,prefix,key_hash,scopes,created_at,\
             expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(input.id)
        .bind(input.organization_id)
        .bind(input.creator_user_id)
        .bind(input.name)
        .bind(input.prefix)
        .bind(input.key_hash)
        .bind(input.scopes)
        .bind(input.created_at)
        .bind(input.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn api_key_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, StoreError> {
        Ok(query_as::<_, ApiKeyRow>(
            "SELECT id,organization_id,creator_user_id,name,prefix,scopes,created_at,last_used_at,\
             expires_at,revoked_at FROM api_keys WHERE key_hash=$1",
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn touch_api_key(&self, id: &str, now: i64) -> Result<(), StoreError> {
        query(
            "UPDATE api_keys SET last_used_at=$1 WHERE id=$2 AND (last_used_at IS NULL OR \
             last_used_at<$3)",
        )
        .bind(now)
        .bind(id)
        .bind(now.saturating_sub(60_000))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn api_keys_for_org(&self, organization_id: &str) -> Result<Vec<ApiKey>, StoreError> {
        Ok(query_as::<_, ApiKeyRow>(
            "SELECT id,organization_id,creator_user_id,name,prefix,scopes,created_at,last_used_at,\
             expires_at,revoked_at FROM api_keys WHERE organization_id=$1 ORDER BY created_at DESC",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// One keyset page of API keys. `creator_user_id` scopes the query for a
    /// regular member; admins and owners pass `None` to see the organization.
    pub async fn api_keys_page(
        &self,
        organization_id: &str,
        creator_user_id: Option<&str>,
        limit: i64,
        after: Option<(i64, &str)>,
    ) -> Result<Vec<ApiKey>, StoreError> {
        const COLUMNS: &str = "id,organization_id,creator_user_id,name,prefix,scopes,created_at,\
                               last_used_at,expires_at,revoked_at";
        let limit = limit.max(1);
        let rows = match (creator_user_id, after) {
            (None, None) => {
                query_as::<_, ApiKeyRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM api_keys WHERE organization_id=$1 ORDER BY created_at \
                     DESC,id DESC LIMIT $2"
                )))
                .bind(organization_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            (None, Some((created_at, id))) => {
                query_as::<_, ApiKeyRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM api_keys WHERE organization_id=$1 AND (created_at,id) \
                     < ($2,$3) ORDER BY created_at DESC,id DESC LIMIT $4"
                )))
                .bind(organization_id)
                .bind(created_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(creator_user_id), None) => {
                query_as::<_, ApiKeyRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM api_keys WHERE organization_id=$1 AND \
                     creator_user_id=$2 ORDER BY created_at DESC,id DESC LIMIT $3"
                )))
                .bind(organization_id)
                .bind(creator_user_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(creator_user_id), Some((created_at, id))) => {
                query_as::<_, ApiKeyRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM api_keys WHERE organization_id=$1 AND \
                     creator_user_id=$2 AND (created_at,id) < ($3,$4) ORDER BY created_at DESC,id \
                     DESC LIMIT $5"
                )))
                .bind(organization_id)
                .bind(creator_user_id)
                .bind(created_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn api_key_for_org(
        &self,
        organization_id: &str,
        key_id: &str,
    ) -> Result<Option<ApiKey>, StoreError> {
        Ok(query_as::<_, ApiKeyRow>(
            "SELECT id,organization_id,creator_user_id,name,prefix,scopes,created_at,last_used_at,\
             expires_at,revoked_at FROM api_keys WHERE organization_id=$1 AND id=$2",
        )
        .bind(organization_id)
        .bind(key_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn revoke_api_key(
        &self,
        organization_id: &str,
        key_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(query(
            "UPDATE api_keys SET revoked_at=$1 WHERE organization_id=$2 AND id=$3 AND revoked_at \
             IS NULL",
        )
        .bind(now_unix_ms())
        .bind(organization_id)
        .bind(key_id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn create_worker_credential(
        &self,
        input: NewWorkerCredential<'_>,
    ) -> Result<(), StoreError> {
        query(
            "INSERT INTO worker_credentials \
             (id,organization_id,owner_user_id,name,token_prefix,token_hash,allowed_namespace,\
             created_at,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(input.id)
        .bind(input.organization_id)
        .bind(input.owner_user_id)
        .bind(input.name)
        .bind(input.token_prefix)
        .bind(input.token_hash)
        .bind(input.allowed_namespace)
        .bind(input.created_at)
        .bind(input.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn worker_credential_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<WorkerCredential>, StoreError> {
        Ok(query_as::<_, WorkerCredentialRow>(
            "SELECT id,organization_id,owner_user_id,name,token_prefix,allowed_namespace,\
             created_at,last_used_at,expires_at,revoked_at FROM worker_credentials WHERE \
             token_hash=$1",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn touch_worker_credential(&self, id: &str, now: i64) -> Result<(), StoreError> {
        query(
            "UPDATE worker_credentials SET last_used_at=$1 WHERE id=$2 AND (last_used_at IS NULL \
             OR last_used_at<$3)",
        )
        .bind(now)
        .bind(id)
        .bind(now.saturating_sub(60_000))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn revoke_worker_credential(
        &self,
        organization_id: &str,
        id: &str,
    ) -> Result<bool, StoreError> {
        Ok(query(
            "UPDATE worker_credentials SET revoked_at=$1 WHERE organization_id=$2 AND id=$3 AND \
             revoked_at IS NULL",
        )
        .bind(now_unix_ms())
        .bind(organization_id)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn worker_credentials_for_org(
        &self,
        organization_id: &str,
    ) -> Result<Vec<WorkerCredential>, StoreError> {
        Ok(query_as::<_, WorkerCredentialRow>(
            "SELECT id,organization_id,owner_user_id,name,token_prefix,allowed_namespace,\
             created_at,last_used_at,expires_at,revoked_at FROM worker_credentials WHERE \
             organization_id=$1 ORDER BY id",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// One keyset page of Worker credentials. Credentials are ordered by their
    /// UUID-like text id so the cursor remains stable when last-used metadata
    /// changes.
    pub async fn worker_credentials_page(
        &self,
        organization_id: &str,
        owner_user_id: Option<&str>,
        limit: i64,
        after: Option<&str>,
    ) -> Result<Vec<WorkerCredential>, StoreError> {
        const COLUMNS: &str = "id,organization_id,owner_user_id,name,token_prefix,\
                               allowed_namespace,created_at,last_used_at,expires_at,revoked_at";
        let limit = limit.max(1);
        let rows = match (owner_user_id, after) {
            (None, None) => {
                query_as::<_, WorkerCredentialRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM worker_credentials WHERE organization_id=$1 ORDER BY \
                     id LIMIT $2"
                )))
                .bind(organization_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            (None, Some(after)) => {
                query_as::<_, WorkerCredentialRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM worker_credentials WHERE organization_id=$1 AND id>$2 \
                     ORDER BY id LIMIT $3"
                )))
                .bind(organization_id)
                .bind(after)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(owner_user_id), None) => {
                query_as::<_, WorkerCredentialRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM worker_credentials WHERE organization_id=$1 AND \
                     owner_user_id=$2 ORDER BY id LIMIT $3"
                )))
                .bind(organization_id)
                .bind(owner_user_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(owner_user_id), Some(after)) => {
                query_as::<_, WorkerCredentialRow>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM worker_credentials WHERE organization_id=$1 AND \
                     owner_user_id=$2 AND id>$3 ORDER BY id LIMIT $4"
                )))
                .bind(organization_id)
                .bind(owner_user_id)
                .bind(after)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn worker_credential_for_org(
        &self,
        organization_id: &str,
        credential_id: &str,
    ) -> Result<Option<WorkerCredential>, StoreError> {
        Ok(query_as::<_, WorkerCredentialRow>(
            "SELECT id,organization_id,owner_user_id,name,token_prefix,allowed_namespace,\
             created_at,last_used_at,expires_at,revoked_at FROM worker_credentials WHERE \
             organization_id=$1 AND id=$2",
        )
        .bind(organization_id)
        .bind(credential_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }
}
