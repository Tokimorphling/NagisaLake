use crate::{rows::*, *};
use sqlx::{AssertSqlSafe, Postgres, Transaction, query, query_as};
use uuid::Uuid;

impl PgStore {
    /// Creates a one-time-or-bounded device sharing code. Only the device
    /// owner may create it; the plaintext is intentionally never persisted.
    pub async fn create_device_invite(&self, input: NewDeviceInvite<'_>) -> Result<(), StoreError> {
        let result = query(
            "INSERT INTO device_share_invites \
             (id,organization_id,device_id,owner_user_id,code_prefix,code_hash,max_uses,\
             expires_at,created_at,allowed_workflows_json,max_concurrent_jobs,\
             grant_duration_seconds) SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12 WHERE EXISTS \
             (SELECT 1 FROM workers w JOIN memberships m ON m.organization_id=w.organization_id \
             AND m.user_id=$4 WHERE w.organization_id=$2 AND w.id=$3 AND (w.owner_user_id=$4 OR \
             w.owner_user_id IS NULL))",
        )
        .bind(input.id)
        .bind(input.organization_id)
        .bind(input.device_id)
        .bind(input.owner_user_id)
        .bind(input.code_prefix)
        .bind(input.code_hash)
        .bind(input.max_uses)
        .bind(input.expires_at)
        .bind(input.created_at)
        .bind(input.allowed_workflows_json)
        .bind(input.max_concurrent_jobs)
        .bind(input.grant_duration_seconds)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(StoreError::NotFound("owned device".into()));
        }
        Ok(())
    }

    pub async fn device_invite_by_hash(
        &self,
        code_hash: &str,
    ) -> Result<Option<DeviceInvite>, StoreError> {
        Ok(query_as::<_, DeviceInviteRow>(
            "SELECT id,organization_id,device_id,owner_user_id,code_prefix,max_uses,use_count,\
             expires_at,revoked_at,created_at,allowed_workflows_json,max_concurrent_jobs,\
             grant_duration_seconds FROM device_share_invites WHERE code_hash=$1",
        )
        .bind(code_hash)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn device_invites_for_owner(
        &self,
        owner_user_id: &str,
        device_id: &str,
    ) -> Result<Vec<DeviceInvite>, StoreError> {
        Ok(query_as::<_, DeviceInviteRow>(
            "SELECT id,organization_id,device_id,owner_user_id,code_prefix,max_uses,use_count,\
             expires_at,revoked_at,created_at,allowed_workflows_json,max_concurrent_jobs,\
             grant_duration_seconds FROM device_share_invites WHERE owner_user_id=$1 AND \
             device_id=$2 ORDER BY created_at DESC",
        )
        .bind(owner_user_id)
        .bind(device_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn device_invite(&self, invite_id: &str) -> Result<Option<DeviceInvite>, StoreError> {
        Ok(query_as::<_, DeviceInviteRow>(
            "SELECT id,organization_id,device_id,owner_user_id,code_prefix,max_uses,use_count,\
             expires_at,revoked_at,created_at,allowed_workflows_json,max_concurrent_jobs,\
             grant_duration_seconds FROM device_share_invites WHERE id=$1",
        )
        .bind(invite_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn revoke_device_invite(
        &self,
        invite_id: &str,
        owner_user_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(query(
            "UPDATE device_share_invites SET revoked_at=$1 WHERE id=$2 AND owner_user_id=$3 AND \
             revoked_at IS NULL",
        )
        .bind(now_unix_ms())
        .bind(invite_id)
        .bind(owner_user_id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    /// Redeems an invite atomically. A grant is scoped to the grantee user,
    /// while jobs remain owned by the consuming user's organization.
    pub async fn accept_device_invite(
        &self,
        code_hash: &str,
        grantee_user_id: &str,
    ) -> Result<DeviceGrant, StoreError> {
        let now = now_unix_ms();
        let mut tx = self.pool.begin().await?;
        let invite = query_as::<_, DeviceInviteRow>(
            "SELECT id,organization_id,device_id,owner_user_id,code_prefix,max_uses,use_count,\
             expires_at,revoked_at,created_at,allowed_workflows_json,max_concurrent_jobs,\
             grant_duration_seconds FROM device_share_invites WHERE code_hash=$1 FOR UPDATE",
        )
        .bind(code_hash)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("device invite".into()))?;
        let existing = query_as::<_, DeviceGrantRow>(
            "SELECT id,device_organization_id,device_id,owner_user_id,grantee_user_id,invite_id,\
             created_at,revoked_at,allowed_workflows_json,max_concurrent_jobs,expires_at FROM \
             device_grants WHERE device_organization_id=$1 AND device_id=$2 AND \
             grantee_user_id=$3 FOR UPDATE",
        )
        .bind(&invite.organization_id)
        .bind(&invite.device_id)
        .bind(grantee_user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing) = existing.as_ref().filter(|grant| {
            grant.revoked_at.is_none() && grant.expires_at.is_none_or(|expires| expires > now)
        }) {
            let grant = DeviceGrant::from(existing.clone());
            tx.commit().await?;
            return Ok(grant);
        }
        if invite.revoked_at.is_some()
            || invite.expires_at.is_some_and(|expires| expires <= now)
            || invite.use_count >= invite.max_uses
        {
            return Err(StoreError::Conflict(
                "device invite is expired, revoked, or exhausted".into(),
            ));
        }
        if invite.owner_user_id == grantee_user_id {
            return Err(StoreError::Conflict(
                "device owner cannot redeem their own invite".into(),
            ));
        }
        let grant_id = existing
            .as_ref()
            .map(|grant| grant.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let grant_expires_at = invite
            .grant_duration_seconds
            .map(|seconds| now.saturating_add(seconds.max(1).saturating_mul(1_000)));
        if existing.is_some() {
            query(
                "UPDATE device_grants SET \
                 revoked_at=NULL,invite_id=$1,created_at=$2,allowed_workflows_json=$3,\
                 max_concurrent_jobs=$4,expires_at=$5 WHERE id=$6",
            )
            .bind(&invite.id)
            .bind(now)
            .bind(&invite.allowed_workflows_json)
            .bind(invite.max_concurrent_jobs)
            .bind(grant_expires_at)
            .bind(&grant_id)
            .execute(&mut *tx)
            .await?;
        } else {
            query(
                "INSERT INTO device_grants \
                 (id,device_organization_id,device_id,owner_user_id,grantee_user_id,invite_id,\
                 created_at,allowed_workflows_json,max_concurrent_jobs,expires_at) VALUES \
                 ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            )
            .bind(&grant_id)
            .bind(&invite.organization_id)
            .bind(&invite.device_id)
            .bind(&invite.owner_user_id)
            .bind(grantee_user_id)
            .bind(&invite.id)
            .bind(now)
            .bind(&invite.allowed_workflows_json)
            .bind(invite.max_concurrent_jobs)
            .bind(grant_expires_at)
            .execute(&mut *tx)
            .await?;
        }
        query("UPDATE device_share_invites SET use_count=use_count+1 WHERE id=$1")
            .bind(&invite.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(DeviceGrant {
            id:                     grant_id,
            device_organization_id: invite.organization_id,
            device_id:              invite.device_id,
            owner_user_id:          invite.owner_user_id,
            grantee_user_id:        grantee_user_id.into(),
            invite_id:              invite.id,
            created_at:             now,
            revoked_at:             None,
            allowed_workflows:      parse_workflow_rules(&invite.allowed_workflows_json),
            max_concurrent_jobs:    invite.max_concurrent_jobs,
            expires_at:             grant_expires_at,
        })
    }

    pub async fn devices_for_user(
        &self,
        user_id: &str,
        organization_id: &str,
    ) -> Result<Vec<DeviceView>, StoreError> {
        let now = now_unix_ms();
        Ok(query_as::<_, DeviceRow>(
            "SELECT w.organization_id,w.id,w.owner_user_id,w.namespace,w.node_name,w.\
             worker_version,w.capabilities_json,'organization_device' AS access_kind,'[]' AS \
             allowed_workflows_json,NULL::BIGINT AS max_concurrent_jobs,NULL::BIGINT AS \
             grant_expires_at FROM workers w WHERE w.organization_id=$2 UNION ALL SELECT \
             w.organization_id,w.id,w.owner_user_id,w.namespace,w.node_name,w.worker_version,w.\
             capabilities_json,'shared_pool_device' AS \
             access_kind,g.allowed_workflows_json,g.max_concurrent_jobs,g.expires_at AS \
             grant_expires_at FROM workers w JOIN device_grants g ON \
             g.device_organization_id=w.organization_id AND g.device_id=w.id WHERE \
             g.grantee_user_id=$1 AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR \
             g.expires_at>$3) AND w.organization_id<>$2 ORDER BY id",
        )
        .bind(user_id)
        .bind(organization_id)
        .bind(now)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    /// One page of devices visible to a user. The composite cursor covers the
    /// organization, device id and access kind because the same id can exist
    /// in more than one organization.
    pub async fn devices_for_user_page(
        &self,
        user_id: &str,
        organization_id: &str,
        limit: i64,
        after: Option<(&str, &str, &str)>,
    ) -> Result<Vec<DeviceView>, StoreError> {
        let now = now_unix_ms();
        const SELECT: &str =
            "SELECT d.organization_id,d.id,d.owner_user_id,d.namespace,d.node_name,d.\
             worker_version,d.capabilities_json,d.access_kind,d.allowed_workflows_json,d.\
             max_concurrent_jobs,d.grant_expires_at FROM (SELECT \
             w.organization_id,w.id,w.owner_user_id,w.namespace,w.node_name,w.worker_version,w.\
             capabilities_json,'organization_device' AS access_kind,'[]' AS \
             allowed_workflows_json,NULL::BIGINT AS max_concurrent_jobs,NULL::BIGINT AS \
             grant_expires_at FROM workers w WHERE w.organization_id=$2 UNION ALL SELECT \
             w.organization_id,w.id,w.owner_user_id,w.namespace,w.node_name,w.worker_version,w.\
             capabilities_json,'shared_pool_device' AS \
             access_kind,g.allowed_workflows_json,g.max_concurrent_jobs,g.expires_at AS \
             grant_expires_at FROM workers w JOIN device_grants g ON \
             g.device_organization_id=w.organization_id AND g.device_id=w.id WHERE \
             g.grantee_user_id=$1 AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR \
             g.expires_at>$3) AND w.organization_id<>$2) d";
        let limit = limit.max(1);
        let rows = match after {
            None => {
                query_as::<_, DeviceRow>(AssertSqlSafe(format!(
                    "{SELECT} ORDER BY d.organization_id,d.id,d.access_kind LIMIT $4"
                )))
                .bind(user_id)
                .bind(organization_id)
                .bind(now)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some((after_organization_id, id, access_kind)) => {
                query_as::<_, DeviceRow>(AssertSqlSafe(format!(
                    "{SELECT} WHERE (d.organization_id,d.id,d.access_kind) > ($4,$5,$6) ORDER BY \
                     d.organization_id,d.id,d.access_kind LIMIT $7"
                )))
                .bind(user_id)
                .bind(organization_id)
                .bind(now)
                .bind(after_organization_id)
                .bind(id)
                .bind(access_kind)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Returns only the stable keys needed to authorize live sessions. The
    /// workflow catalog uses this instead of loading every device capability
    /// document into memory.
    pub async fn device_access_for_user(
        &self,
        user_id: &str,
        organization_id: &str,
    ) -> Result<Vec<DeviceAccess>, StoreError> {
        let now = now_unix_ms();
        Ok(query_as::<_, DeviceAccessRow>(
            "SELECT w.organization_id AS device_organization_id,w.id AS \
             device_id,'organization_device' AS access_kind,'[]' AS \
             allowed_workflows_json,NULL::BIGINT AS max_concurrent_jobs,NULL::BIGINT AS \
             grant_expires_at FROM workers w WHERE w.organization_id=$2 UNION ALL SELECT \
             w.organization_id AS device_organization_id,w.id AS device_id,'shared_pool_device' \
             AS access_kind,g.allowed_workflows_json,g.max_concurrent_jobs,g.expires_at AS \
             grant_expires_at FROM workers w JOIN device_grants g ON \
             g.device_organization_id=w.organization_id AND g.device_id=w.id WHERE \
             g.grantee_user_id=$1 AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR \
             g.expires_at>$3) AND w.organization_id<>$2 ORDER BY device_organization_id,device_id",
        )
        .bind(user_id)
        .bind(organization_id)
        .bind(now)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn devices_for_org(
        &self,
        organization_id: &str,
    ) -> Result<Vec<DeviceView>, StoreError> {
        Ok(query_as::<_, DeviceRow>(
            "SELECT organization_id,id,owner_user_id,namespace,node_name,worker_version,\
             capabilities_json,'organization_device' AS access_kind,'[]' AS \
             allowed_workflows_json,NULL::BIGINT AS max_concurrent_jobs,NULL::BIGINT AS \
             grant_expires_at FROM workers WHERE organization_id=$1 ORDER BY id",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn shareable_device_for_user(
        &self,
        organization_id: &str,
        device_id: &str,
        user_id: &str,
    ) -> Result<Option<DeviceView>, StoreError> {
        Ok(query_as::<_, DeviceRow>(
            "SELECT w.organization_id,w.id,w.owner_user_id,w.namespace,w.node_name,w.\
             worker_version,w.capabilities_json,'organization_device' AS access_kind,'[]' AS \
             allowed_workflows_json,NULL::BIGINT AS max_concurrent_jobs,NULL::BIGINT AS \
             grant_expires_at FROM workers w JOIN memberships m ON \
             m.organization_id=w.organization_id AND m.user_id=$3 WHERE w.organization_id=$1 AND \
             w.id=$2 AND (w.owner_user_id=$3 OR w.owner_user_id IS NULL)",
        )
        .bind(organization_id)
        .bind(device_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn revoke_device_grant(
        &self,
        device_organization_id: &str,
        device_id: &str,
        owner_user_id: &str,
        grantee_user_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(query(
            "UPDATE device_grants SET revoked_at=$1 WHERE device_organization_id=$2 AND \
             device_id=$3 AND owner_user_id=$4 AND grantee_user_id=$5 AND revoked_at IS NULL",
        )
        .bind(now_unix_ms())
        .bind(device_organization_id)
        .bind(device_id)
        .bind(owner_user_id)
        .bind(grantee_user_id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn can_use_device(
        &self,
        user_id: &str,
        organization_id: &str,
        device_organization_id: &str,
        device_id: &str,
    ) -> Result<bool, StoreError> {
        let now = now_unix_ms();
        Ok(query_as::<_, (bool,)>(
            "SELECT ($2=$3 AND EXISTS (SELECT 1 FROM workers WHERE organization_id=$3 AND id=$4)) \
             OR EXISTS (SELECT 1 FROM device_grants WHERE device_organization_id=$3 AND \
             device_id=$4 AND grantee_user_id=$1 AND revoked_at IS NULL AND (expires_at IS NULL \
             OR expires_at>$5))",
        )
        .bind(user_id)
        .bind(organization_id)
        .bind(device_organization_id)
        .bind(device_id)
        .bind(now)
        .fetch_one(&self.pool)
        .await?
        .0)
    }

    pub async fn upsert_worker(&self, input: WorkerUpsert<'_>) -> Result<(), StoreError> {
        let result = query(
            "INSERT INTO workers \
             (organization_id,id,owner_user_id,namespace,node_name,worker_version,\
             capabilities_json,last_session_id,last_seen_at,created_at,updated_at) VALUES \
             ($1,$2,$3,$4,$5,$6,$7,$8,$9,$9,$9) ON CONFLICT (organization_id,id) DO UPDATE SET \
             namespace=EXCLUDED.namespace,node_name=EXCLUDED.node_name,worker_version=EXCLUDED.\
             worker_version,capabilities_json=EXCLUDED.capabilities_json,last_session_id=EXCLUDED.\
             last_session_id,last_seen_at=EXCLUDED.last_seen_at,updated_at=EXCLUDED.updated_at \
             WHERE workers.owner_user_id IS NOT DISTINCT FROM EXCLUDED.owner_user_id",
        )
        .bind(input.organization_id)
        .bind(input.id)
        .bind(input.owner_user_id)
        .bind(input.namespace)
        .bind(input.node_name)
        .bind(input.worker_version)
        .bind(input.capabilities_json)
        .bind(input.session_id)
        .bind(input.now)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(StoreError::Conflict(
                "worker identity belongs to another user".into(),
            ));
        }
        Ok(())
    }

    /// Updates the durable worker heartbeat used by reconciliation. A stale
    /// socket must not refresh a replacement session's liveness.
    pub async fn touch_worker_heartbeat(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE workers SET last_seen_at=$1,updated_at=$1 WHERE organization_id=$2 AND id=$3 \
             AND last_session_id=$4",
        )
        .bind(now)
        .bind(organization_id)
        .bind(worker_id)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

pub(crate) async fn enforce_device_use_policy_tx(
    tx: &mut Transaction<'_, Postgres>,
    admission: &DeviceUseAdmission<'_>,
) -> Result<(), StoreError> {
    let requested_jobs = admission.requested_jobs.max(1);
    if admission.device_organization_id == admission.organization_id {
        let exists = query_as::<_, (bool,)>(
            "SELECT EXISTS (SELECT 1 FROM workers WHERE organization_id=$1 AND id=$2)",
        )
        .bind(admission.device_organization_id)
        .bind(admission.device_id)
        .fetch_one(&mut **tx)
        .await?
        .0;
        if !exists {
            return Err(StoreError::NotFound("organization device".into()));
        }
        return Ok(());
    }

    let grant = query_as::<_, (String, Option<i64>, Option<i64>)>(
        "SELECT allowed_workflows_json,max_concurrent_jobs,expires_at FROM device_grants WHERE \
         device_organization_id=$1 AND device_id=$2 AND grantee_user_id=$3 AND revoked_at IS NULL \
         AND (expires_at IS NULL OR expires_at>$4) FOR UPDATE",
    )
    .bind(admission.device_organization_id)
    .bind(admission.device_id)
    .bind(admission.user_id)
    .bind(admission.now)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::Conflict("shared pool device grant is not active".into()))?;

    let rules: Vec<DeviceWorkflowRule> = serde_json::from_str(&grant.0).map_err(|error| {
        StoreError::InvalidConfig(format!("invalid device grant policy: {error}"))
    })?;
    if !device_workflow_allowed(&rules, admission.workflow_id, admission.workflow_version) {
        return Err(StoreError::Conflict(
            "shared pool device grant does not allow this workflow version".into(),
        ));
    }

    if let Some(limit) = grant.1 {
        let active = query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM jobs j LEFT JOIN job_batches b ON \
             b.organization_id=j.organization_id AND b.id=j.batch_id WHERE j.organization_id=$1 \
             AND j.actor_user_id=$2 AND j.state NOT IN ('completed','failed','cancelled') AND \
             ((j.worker_organization_id=$3 AND j.worker_id=$4) OR (j.state='queued' AND \
             b.device_organization_id=$3 AND b.device_id=$4))",
        )
        .bind(admission.organization_id)
        .bind(admission.user_id)
        .bind(admission.device_organization_id)
        .bind(admission.device_id)
        .fetch_one(&mut **tx)
        .await?
        .0;
        if active.saturating_add(requested_jobs) > limit {
            return Err(StoreError::QuotaExceeded(
                "shared_device_concurrency".into(),
            ));
        }
    }

    Ok(())
}
