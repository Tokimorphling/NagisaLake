use crate::{rows::*, *};
use nagisalake_hub_auth::Role;
use sqlx::{query, query_as};
use uuid::Uuid;

impl PgStore {
    pub async fn ensure_organization(
        &self,
        organization_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        let now = now_unix_ms();
        let mut tx = self.pool.begin().await?;
        query(
            "INSERT INTO organizations (id, name, status, created_at, updated_at) VALUES ($1, $2, \
             'active', $3, $3) ON CONFLICT (id) DO NOTHING",
        )
        .bind(organization_id)
        .bind(name)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO quota_policies (organization_id, max_concurrent_jobs, updated_at) VALUES \
             ($1, $2, $3) ON CONFLICT (organization_id) DO NOTHING",
        )
        .bind(organization_id)
        .bind(DEFAULT_MAX_CONCURRENT_JOBS)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO quota_usage (organization_id, period_started_at, updated_at) VALUES ($1, \
             $2, $2) ON CONFLICT (organization_id) DO NOTHING",
        )
        .bind(organization_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn memberships_for_user(&self, user_id: &str) -> Result<Vec<Membership>, StoreError> {
        Ok(query_as::<_, MembershipRow>(
            "SELECT m.organization_id,m.user_id,m.role,o.name FROM memberships m JOIN \
             organizations o ON o.id=m.organization_id WHERE m.user_id=$1 ORDER BY o.name",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn membership(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Option<Membership>, StoreError> {
        Ok(query_as::<_, MembershipRow>(
            "SELECT m.organization_id,m.user_id,m.role,o.name FROM memberships m JOIN \
             organizations o ON o.id=m.organization_id WHERE m.organization_id=$1 AND m.user_id=$2",
        )
        .bind(organization_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn organization(
        &self,
        organization_id: &str,
    ) -> Result<Option<Organization>, StoreError> {
        Ok(query_as::<_, OrganizationRow>(
            "SELECT id,name,status,created_at,updated_at FROM organizations WHERE id=$1",
        )
        .bind(organization_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn create_organization_for_user(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<Membership, StoreError> {
        let organization_id = Uuid::new_v4().to_string();
        let now = now_unix_ms();
        let mut tx = self.pool.begin().await?;
        query(
            "INSERT INTO organizations (id,name,status,created_at,updated_at) VALUES \
             ($1,$2,'active',$3,$3)",
        )
        .bind(&organization_id)
        .bind(name)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO memberships (organization_id,user_id,role,created_at,updated_at) VALUES \
             ($1,$2,'owner',$3,$3)",
        )
        .bind(&organization_id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO quota_policies (organization_id,max_concurrent_jobs,updated_at) VALUES \
             ($1,$2,$3)",
        )
        .bind(&organization_id)
        .bind(DEFAULT_MAX_CONCURRENT_JOBS)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO quota_usage (organization_id,period_started_at,updated_at) VALUES \
             ($1,$2,$2)",
        )
        .bind(&organization_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Membership {
            organization_id,
            user_id: user_id.into(),
            role: Role::Owner,
            organization_name: name.into(),
        })
    }

    pub async fn members_for_org(
        &self,
        organization_id: &str,
    ) -> Result<Vec<OrganizationMember>, StoreError> {
        Ok(query_as::<_, OrganizationMemberRow>(
            "SELECT m.organization_id,m.user_id,u.email,m.role,m.created_at FROM memberships m \
             JOIN users u ON u.id=m.user_id WHERE m.organization_id=$1 ORDER BY m.created_at",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn set_member_role(
        &self,
        organization_id: &str,
        user_id: &str,
        role: Role,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let memberships = query_as::<_, (String, String)>(
            "SELECT user_id,role FROM memberships WHERE organization_id=$1 ORDER BY user_id FOR \
             UPDATE",
        )
        .bind(organization_id)
        .fetch_all(&mut *tx)
        .await?;
        let Some((_, current_role)) = memberships.iter().find(|(id, _)| id == user_id) else {
            tx.rollback().await?;
            return Ok(false);
        };
        if current_role == "owner"
            && role != Role::Owner
            && memberships
                .iter()
                .filter(|(_, current)| current == "owner")
                .count()
                == 1
        {
            return Err(StoreError::Conflict(
                "the last organization owner cannot be demoted".into(),
            ));
        }
        query(
            "UPDATE memberships SET role=$1,updated_at=$2 WHERE organization_id=$3 AND user_id=$4",
        )
        .bind(role.to_string())
        .bind(now_unix_ms())
        .bind(organization_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn create_organization_invite(
        &self,
        input: NewOrganizationInvite<'_>,
    ) -> Result<(), StoreError> {
        query(
            "INSERT INTO organization_invites \
             (id,organization_id,inviter_user_id,code_prefix,code_hash,role,created_at,\
             expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(input.id)
        .bind(input.organization_id)
        .bind(input.inviter_user_id)
        .bind(input.code_prefix)
        .bind(input.code_hash)
        .bind(input.role.to_string())
        .bind(input.created_at)
        .bind(input.expires_at)
        .execute(&self.pool)
        .await
        .map_err(map_unique)?;
        Ok(())
    }

    pub async fn organization_invites(
        &self,
        organization_id: &str,
    ) -> Result<Vec<OrganizationInvite>, StoreError> {
        Ok(query_as::<_, OrganizationInviteRow>(
            "SELECT id,organization_id,inviter_user_id,code_prefix,role,created_at,expires_at,\
             accepted_at,accepted_by_user_id,revoked_at FROM organization_invites WHERE \
             organization_id=$1 ORDER BY created_at DESC",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
    }

    pub async fn revoke_organization_invite(
        &self,
        organization_id: &str,
        invite_id: &str,
    ) -> Result<bool, StoreError> {
        let result = query(
            "UPDATE organization_invites SET revoked_at=$1 WHERE organization_id=$2 AND id=$3 AND \
             revoked_at IS NULL AND accepted_at IS NULL",
        )
        .bind(now_unix_ms())
        .bind(organization_id)
        .bind(invite_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Consumes an invitation and creates the membership in one transaction.
    /// Existing members keep their current role so accepting an old invite can
    /// never silently demote them.
    pub async fn accept_organization_invite(
        &self,
        code_hash: &str,
        user_id: &str,
    ) -> Result<Membership, StoreError> {
        let now = now_unix_ms();
        let mut tx = self.pool.begin().await?;
        let invite = query_as::<_, (String, String, i64, Option<i64>, Option<i64>)>(
            "SELECT id,organization_id,expires_at,accepted_at,revoked_at FROM \
             organization_invites WHERE code_hash=$1 FOR UPDATE",
        )
        .bind(code_hash)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("organization invitation".into()))?;
        let (invite_id, organization_id, expires_at, accepted_at, revoked_at) = invite;
        if accepted_at.is_some() || revoked_at.is_some() || expires_at <= now {
            return Err(StoreError::Conflict(
                "organization invitation is expired, used, or revoked".into(),
            ));
        }
        let invite_role =
            query_as::<_, (String,)>("SELECT role FROM organization_invites WHERE id=$1")
                .bind(&invite_id)
                .fetch_one(&mut *tx)
                .await?
                .0;
        let existing = query_as::<_, (String,)>(
            "SELECT role FROM memberships WHERE organization_id=$1 AND user_id=$2",
        )
        .bind(&organization_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let role = match existing {
            Some((role,)) => role,
            None => {
                query(
                    "INSERT INTO memberships (organization_id,user_id,role,created_at,updated_at) \
                     VALUES ($1,$2,$3,$4,$4)",
                )
                .bind(&organization_id)
                .bind(user_id)
                .bind(&invite_role)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                invite_role
            }
        };
        query("UPDATE organization_invites SET accepted_at=$1,accepted_by_user_id=$2 WHERE id=$3")
            .bind(now)
            .bind(user_id)
            .bind(&invite_id)
            .execute(&mut *tx)
            .await?;
        let organization_name =
            query_as::<_, (String,)>("SELECT name FROM organizations WHERE id=$1")
                .bind(&organization_id)
                .fetch_one(&mut *tx)
                .await?
                .0;
        tx.commit().await?;
        Ok(Membership {
            organization_id,
            user_id: user_id.into(),
            role: role.parse().unwrap_or(Role::Viewer),
            organization_name,
        })
    }

    pub async fn remove_member(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let memberships = query_as::<_, (String, String)>(
            "SELECT user_id,role FROM memberships WHERE organization_id=$1 ORDER BY user_id FOR \
             UPDATE",
        )
        .bind(organization_id)
        .fetch_all(&mut *tx)
        .await?;
        let Some((_, target_role)) = memberships.iter().find(|(id, _)| id == user_id) else {
            tx.rollback().await?;
            return Ok(None);
        };
        if target_role == "owner"
            && memberships
                .iter()
                .filter(|(_, role)| role == "owner")
                .count()
                == 1
        {
            return Err(StoreError::Conflict(
                "the last organization owner cannot be removed".into(),
            ));
        }
        let credential_ids = query_as::<_, (String,)>(
            "SELECT id FROM worker_credentials WHERE organization_id=$1 AND owner_user_id=$2 FOR \
             UPDATE",
        )
        .bind(organization_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect::<Vec<_>>();
        query(
            "UPDATE worker_credentials SET revoked_at=COALESCE(revoked_at,$1) WHERE \
             organization_id=$2 AND owner_user_id=$3",
        )
        .bind(now_unix_ms())
        .bind(organization_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
            .bind(organization_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(credential_ids))
    }

    /// Makes `to_user_id` an owner and demotes the initiating owner to admin in
    /// one transaction, so the organization never passes through a no-owner
    /// state.
    pub async fn transfer_owner(
        &self,
        organization_id: &str,
        from_user_id: &str,
        to_user_id: &str,
    ) -> Result<(), StoreError> {
        if from_user_id == to_user_id {
            return Err(StoreError::Conflict(
                "owner transfer target must be another member".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        let memberships = query_as::<_, (String, String)>(
            "SELECT user_id,role FROM memberships WHERE organization_id=$1 ORDER BY user_id FOR \
             UPDATE",
        )
        .bind(organization_id)
        .fetch_all(&mut *tx)
        .await?;
        if !memberships
            .iter()
            .any(|(id, role)| id == from_user_id && role == "owner")
        {
            return Err(StoreError::Conflict(
                "owner transfer initiator is not an owner".into(),
            ));
        }
        if !memberships.iter().any(|(id, _)| id == to_user_id) {
            return Err(StoreError::NotFound("owner transfer target".into()));
        }
        query(
            "UPDATE memberships SET role=CASE WHEN user_id=$1 THEN 'owner' ELSE 'admin' \
             END,updated_at=$2 WHERE organization_id=$3 AND user_id IN ($1,$4)",
        )
        .bind(to_user_id)
        .bind(now_unix_ms())
        .bind(organization_id)
        .bind(from_user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Returns the persisted lock deadline when an account is currently
    /// locked. Expired locks are treated as inactive but left for the next
    /// failed-attempt transaction to clear atomically.
    /// Deletes all durable metadata for an organization. Audit rows are not
    /// foreign-keyed so remove them explicitly before the organization cascade.
    pub async fn delete_organization(&self, organization_id: &str) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        query("DELETE FROM audit_logs WHERE organization_id=$1")
            .bind(organization_id)
            .execute(&mut *tx)
            .await?;
        let deleted = query("DELETE FROM organizations WHERE id=$1")
            .bind(organization_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(deleted == 1)
    }
}
