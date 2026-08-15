use crate::{rows::*, *};
use sqlx::{query, query_as};

impl PgStore {
    /// Returns the persisted lock deadline when an account is currently
    /// locked. Expired locks are treated as inactive but left for the next
    /// failed-attempt transaction to clear atomically.
    pub async fn login_lock_until(
        &self,
        user_id: &str,
        now: i64,
    ) -> Result<Option<i64>, StoreError> {
        Ok(query_as::<_, (Option<i64>,)>(
            "SELECT locked_until FROM users WHERE id=$1 AND locked_until > $2",
        )
        .bind(user_id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?
        .and_then(|(until,)| until))
    }

    /// Records one failed password attempt and persists a temporary account
    /// lock after `max_attempts`. The row lock makes concurrent guesses count
    /// exactly once even when they arrive on different Tokio tasks.
    pub async fn record_failed_login(
        &self,
        user_id: &str,
        now: i64,
        max_attempts: i64,
        lock_seconds: i64,
    ) -> Result<Option<i64>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let Some((attempts, locked_until)) = query_as::<_, (i64, Option<i64>)>(
            "SELECT failed_login_attempts,locked_until FROM users WHERE id=$1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            tx.rollback().await?;
            return Ok(None);
        };

        if locked_until.is_some_and(|until| until > now) {
            tx.rollback().await?;
            return Ok(locked_until);
        }

        let attempts = if locked_until.is_some() { 0 } else { attempts } + 1;
        if attempts >= max_attempts.max(1) {
            let until = now.saturating_add(lock_seconds.max(1).saturating_mul(1_000));
            query(
                "UPDATE users SET failed_login_attempts=0,locked_until=$1,updated_at=$2 WHERE \
                 id=$3",
            )
            .bind(until)
            .bind(now)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Some(until));
        }
        query(
            "UPDATE users SET failed_login_attempts=$1,locked_until=NULL,updated_at=$2 WHERE id=$3",
        )
        .bind(attempts)
        .bind(now)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(None)
    }

    pub async fn clear_failed_logins(&self, user_id: &str) -> Result<(), StoreError> {
        query("UPDATE users SET failed_login_attempts=0,locked_until=NULL WHERE id=$1")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_session(&self, input: NewSession<'_>) -> Result<(), StoreError> {
        query(
            "INSERT INTO browser_sessions \
             (id,user_id,organization_id,access_token_hash,refresh_token_hash,csrf_token_hash,\
             family_id,created_at,last_seen_at,access_expires_at,refresh_expires_at,\
             user_agent_hash,ip_hash) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$8,$9,$10,$11,$12)",
        )
        .bind(input.id)
        .bind(input.user_id)
        .bind(input.organization_id)
        .bind(input.access_token_hash)
        .bind(input.refresh_token_hash)
        .bind(input.csrf_token_hash)
        .bind(input.family_id)
        .bind(input.now)
        .bind(input.access_expires_at)
        .bind(input.refresh_expires_at)
        .bind(input.user_agent_hash)
        .bind(input.ip_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn session_by_access_hash(
        &self,
        hash: &str,
    ) -> Result<Option<BrowserSession>, StoreError> {
        Ok(query_as::<_, SessionRow>(
            "SELECT id,user_id,organization_id,family_id,csrf_token_hash,access_expires_at,\
             refresh_expires_at,revoked_at FROM browser_sessions WHERE access_token_hash=$1",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn session_by_refresh_hash(
        &self,
        hash: &str,
    ) -> Result<Option<BrowserSession>, StoreError> {
        Ok(query_as::<_, SessionRow>(
            "SELECT id,user_id,organization_id,family_id,csrf_token_hash,access_expires_at,\
             refresh_expires_at,revoked_at FROM browser_sessions WHERE refresh_token_hash=$1",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn rotate_session(&self, input: RotateSession<'_>) -> Result<bool, StoreError> {
        let result = query(
            "UPDATE browser_sessions SET \
             access_token_hash=$1,refresh_token_hash=$2,last_seen_at=$3,access_expires_at=$4,\
             refresh_expires_at=$5 WHERE id=$6 AND refresh_token_hash=$7 AND revoked_at IS NULL",
        )
        .bind(input.access_token_hash)
        .bind(input.refresh_token_hash)
        .bind(input.now)
        .bind(input.access_expires_at)
        .bind(input.refresh_expires_at)
        .bind(input.session_id)
        .bind(input.expected_refresh_token_hash)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn revoke_session(&self, session_id: &str) -> Result<(), StoreError> {
        query("UPDATE browser_sessions SET revoked_at=$1 WHERE id=$2 AND revoked_at IS NULL")
            .bind(now_unix_ms())
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn revoke_user_sessions(
        &self,
        user_id: &str,
        except: Option<&str>,
    ) -> Result<(), StoreError> {
        if let Some(except) = except {
            query(
                "UPDATE browser_sessions SET revoked_at=$1 WHERE user_id=$2 AND id<>$3 AND \
                 revoked_at IS NULL",
            )
            .bind(now_unix_ms())
            .bind(user_id)
            .bind(except)
            .execute(&self.pool)
            .await?;
        } else {
            query(
                "UPDATE browser_sessions SET revoked_at=$1 WHERE user_id=$2 AND revoked_at IS NULL",
            )
            .bind(now_unix_ms())
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Deletes an account after ownership has been transferred or its
    /// organizations have been deleted. Personal worker credentials and owned
    /// devices are removed with it; shared organization data remains intact.
    pub async fn delete_user(&self, user_id: &str) -> Result<Vec<String>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let memberships = query_as::<_, (String, String)>(
            "SELECT organization_id,role FROM memberships WHERE user_id=$1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        if memberships.iter().any(|(_, role)| role == "owner") {
            return Err(StoreError::Conflict(
                "transfer ownership or delete owned organizations before deleting the account"
                    .into(),
            ));
        }
        let credential_ids = query_as::<_, (String,)>(
            "SELECT id FROM worker_credentials WHERE owner_user_id=$1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect();
        // Audit rows intentionally outlive an account, but no longer retain a
        // direct actor identifier after the account is erased.
        query("UPDATE audit_logs SET actor_id=NULL WHERE actor_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        query("DELETE FROM workers WHERE owner_user_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        query("DELETE FROM worker_credentials WHERE owner_user_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let deleted = query("DELETE FROM users WHERE id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted == 0 {
            return Err(StoreError::NotFound("user".into()));
        }
        tx.commit().await?;
        Ok(credential_ids)
    }
}
