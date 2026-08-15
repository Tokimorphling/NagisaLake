use crate::{rows::*, *};
use sqlx::{query, query_as};
use uuid::Uuid;

impl PgStore {
    pub async fn register_user(
        &self,
        email: &str,
        password_hash: &str,
        organization_name: &str,
    ) -> Result<RegisteredAccount, StoreError> {
        self.create_account(email, Some(password_hash), organization_name, None)
            .await
    }

    /// Creates a user, their first organization, and the owner membership.
    ///
    /// `password_hash` is `None` for a federated account: there is no password to
    /// verify, and storing a synthetic hash would look like a usable credential.
    /// `verified_at` marks the address as confirmed, which only a provider that
    /// asserted verification may set.
    async fn create_account(
        &self,
        email: &str,
        password_hash: Option<&str>,
        organization_name: &str,
        verified_at: Option<i64>,
    ) -> Result<RegisteredAccount, StoreError> {
        let id = Uuid::new_v4().to_string();
        let organization_id = Uuid::new_v4().to_string();
        let now = now_unix_ms();
        let normalized = normalize_email(email);
        let mut tx = self.pool.begin().await?;
        let user = query_as::<_, UserRow>(
            "INSERT INTO users \
             (id,email,email_normalized,password_hash,status,email_verified_at,created_at,\
             updated_at) VALUES ($1,$2,$3,$4,'active',$5,$6,$6) RETURNING \
             id,email,password_hash,status,email_verified_at,created_at,updated_at",
        )
        .bind(&id)
        .bind(email.trim())
        .bind(&normalized)
        .bind(password_hash)
        .bind(verified_at)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_unique)?;
        query(
            "INSERT INTO organizations (id,name,status,created_at,updated_at) VALUES \
             ($1,$2,'active',$3,$3)",
        )
        .bind(&organization_id)
        .bind(organization_name)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO memberships (organization_id,user_id,role,created_at,updated_at) VALUES \
             ($1,$2,'owner',$3,$3)",
        )
        .bind(&organization_id)
        .bind(&id)
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
        Ok(RegisteredAccount {
            user: user.into(),
            organization_id,
        })
    }

    /// Resolves a federated sign-in to a local account, creating or linking as
    /// needed. Returns the account plus how it was resolved.
    ///
    /// Three cases, in order:
    ///
    /// 1. The `(provider, subject)` pair is already linked. The subject is the
    ///    provider's immutable id, so this path is unaffected by the user
    ///    changing their email at the provider.
    /// 2. No link, but a local account holds the same address. Linking here is
    ///    only sound when the provider asserted the address was verified —
    ///    GitHub lets anyone attach an arbitrary address to their account, so an
    ///    unverified match would let an attacker claim someone else's account.
    /// 3. Neither. A fresh account and organization are created.
    ///
    /// The whole resolution runs in one transaction, so two concurrent callbacks
    /// for the same subject cannot produce two accounts.
    pub async fn resolve_federated_identity(
        &self,
        provider: &str,
        subject: &str,
        email: &str,
        email_verified: bool,
        organization_name: &str,
    ) -> Result<FederatedLogin, StoreError> {
        let now = now_unix_ms();
        let normalized = normalize_email(email);
        let mut tx = self.pool.begin().await?;

        // Case 1: already linked.
        let linked = query_as::<_, UserRow>(
            "SELECT u.id,u.email,u.password_hash,u.status,u.email_verified_at,u.created_at,u.\
             updated_at FROM users u JOIN user_identities i ON i.user_id = u.id WHERE i.provider \
             = $1 AND i.subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(user) = linked {
            query(
                "UPDATE user_identities SET last_login_at=$1,email=$2,email_verified=$3 WHERE \
                 provider=$4 AND subject=$5",
            )
            .bind(now)
            .bind(email.trim())
            .bind(email_verified)
            .bind(provider)
            .bind(subject)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(FederatedLogin {
                user:    user.into(),
                outcome: FederatedOutcome::Existing,
            });
        }

        // Case 2: an account already owns this address.
        let existing = query_as::<_, UserRow>(
            "SELECT id,email,password_hash,status,email_verified_at,created_at,updated_at FROM \
             users WHERE email_normalized = $1",
        )
        .bind(&normalized)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(user) = existing {
            if !email_verified {
                // Refusing is the only safe answer: accepting would let anyone
                // who can add this address at the provider take the account.
                return Err(StoreError::Conflict(format!(
                    "{provider} did not verify {email}, so it cannot be linked to the existing \
                     account with that address"
                )));
            }
            query(
                "INSERT INTO user_identities \
                 (provider,subject,user_id,email,email_verified,created_at,last_login_at) VALUES \
                 ($1,$2,$3,$4,$5,$6,$6)",
            )
            .bind(provider)
            .bind(subject)
            .bind(&user.id)
            .bind(email.trim())
            .bind(email_verified)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|error| match map_unique(error) {
                // The unique index on (user_id, provider) rejected a second
                // account from the same provider.
                StoreError::Conflict(_) => StoreError::Conflict(format!(
                    "this account is already linked to a different {provider} identity"
                )),
                other => other,
            })?;
            // A provider-verified address confirms an address this deployment
            // could not verify on its own.
            query(
                "UPDATE users SET email_verified_at=COALESCE(email_verified_at,$1),updated_at=$1 \
                 WHERE id=$2",
            )
            .bind(now)
            .bind(&user.id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            let mut user: User = user.into();
            user.email_verified_at = user.email_verified_at.or(Some(now));
            return Ok(FederatedLogin {
                user,
                outcome: FederatedOutcome::Linked,
            });
        }
        // Case 3: brand new. User, organization, membership, quota and provider
        // identity must commit together. Otherwise a conflict on the final
        // identity insert can strand an account that can never sign in.
        let user_id = Uuid::new_v4().to_string();
        let organization_id = Uuid::new_v4().to_string();
        let user = query_as::<_, UserRow>(
            "INSERT INTO users \
             (id,email,email_normalized,password_hash,status,email_verified_at,created_at,\
             updated_at) VALUES ($1,$2,$3,$4,'active',$5,$6,$6) RETURNING \
             id,email,password_hash,status,email_verified_at,created_at,updated_at",
        )
        .bind(&user_id)
        .bind(email.trim())
        .bind(&normalized)
        .bind(Option::<&str>::None)
        .bind(email_verified.then_some(now))
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_unique)?;
        query(
            "INSERT INTO organizations (id,name,status,created_at,updated_at) VALUES \
             ($1,$2,'active',$3,$3)",
        )
        .bind(&organization_id)
        .bind(organization_name)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        query(
            "INSERT INTO memberships (organization_id,user_id,role,created_at,updated_at) VALUES \
             ($1,$2,'owner',$3,$3)",
        )
        .bind(&organization_id)
        .bind(&user_id)
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
        query(
            "INSERT INTO user_identities \
             (provider,subject,user_id,email,email_verified,created_at,last_login_at) VALUES \
             ($1,$2,$3,$4,$5,$6,$6)",
        )
        .bind(provider)
        .bind(subject)
        .bind(&user_id)
        .bind(email.trim())
        .bind(email_verified)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_unique)?;
        tx.commit().await?;
        Ok(FederatedLogin {
            user:    user.into(),
            outcome: FederatedOutcome::Created { organization_id },
        })
    }

    /// Providers linked to an account, for display on the settings page.
    pub async fn identities_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<LinkedIdentity>, StoreError> {
        Ok(
            query_as::<_, (String, Option<String>, bool, i64, Option<i64>)>(
                "SELECT provider,email,email_verified,created_at,last_login_at FROM \
                 user_identities WHERE user_id=$1 ORDER BY provider",
            )
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(
                |(provider, email, email_verified, created_at, last_login_at)| LinkedIdentity {
                    provider,
                    email,
                    email_verified,
                    created_at,
                    last_login_at,
                },
            )
            .collect(),
        )
    }

    /// Stores a pending authorization request.
    pub async fn create_oauth_authorization(
        &self,
        state: &str,
        provider: &str,
        pkce_verifier: &str,
        redirect_path: &str,
        ttl_seconds: i64,
    ) -> Result<(), StoreError> {
        let now = now_unix_ms();
        query(
            "INSERT INTO oauth_authorizations \
             (state,provider,pkce_verifier,redirect_path,created_at,expires_at) VALUES \
             ($1,$2,$3,$4,$5,$6)",
        )
        .bind(state)
        .bind(provider)
        .bind(pkce_verifier)
        .bind(redirect_path)
        .bind(now)
        .bind(now + ttl_seconds * 1_000)
        .execute(&self.pool)
        .await
        .map_err(map_unique)?;
        Ok(())
    }

    /// Consumes an authorization request, returning it only the first time.
    ///
    /// The conditional UPDATE makes this single-use even under a replayed
    /// callback: the second attempt matches no row.
    pub async fn consume_oauth_authorization(
        &self,
        state: &str,
    ) -> Result<Option<OauthAuthorization>, StoreError> {
        let now = now_unix_ms();
        Ok(query_as::<_, (String, String, String)>(
            "UPDATE oauth_authorizations SET consumed_at=$1 WHERE state=$2 AND consumed_at IS \
             NULL AND expires_at > $1 RETURNING provider,pkce_verifier,redirect_path",
        )
        .bind(now)
        .bind(state)
        .fetch_optional(&self.pool)
        .await?
        .map(
            |(provider, pkce_verifier, redirect_path)| OauthAuthorization {
                provider,
                pkce_verifier,
                redirect_path,
            },
        ))
    }

    /// Deletes expired or consumed authorization requests.
    pub async fn prune_oauth_authorizations(&self) -> Result<u64, StoreError> {
        let now = now_unix_ms();
        Ok(query(
            "DELETE FROM oauth_authorizations WHERE expires_at <= $1 OR (consumed_at IS NOT NULL \
             AND consumed_at <= $2)",
        )
        .bind(now)
        // Keep consumed rows briefly so a duplicate callback still reports
        // "already used" rather than "unknown state".
        .bind(now - 600_000)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    pub async fn user_by_email(&self, email: &str) -> Result<Option<User>, StoreError> {
        Ok(query_as::<_, UserRow>(
            "SELECT id,email,password_hash,status,email_verified_at,created_at,updated_at FROM \
             users WHERE email_normalized=$1",
        )
        .bind(normalize_email(email))
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    /// Looks up an already-linked provider subject without creating or linking
    /// anything. Closed-registration OAuth callbacks use this path so a denied
    /// sign-in cannot leave behind a new account.
    pub async fn user_by_federated_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<User>, StoreError> {
        Ok(query_as::<_, UserRow>(
            "SELECT u.id,u.email,u.password_hash,u.status,u.email_verified_at,u.created_at,u.\
             updated_at FROM users u JOIN user_identities i ON i.user_id=u.id WHERE i.provider=$1 \
             AND i.subject=$2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }

    pub async fn user_by_id(&self, user_id: &str) -> Result<Option<User>, StoreError> {
        Ok(query_as::<_, UserRow>(
            "SELECT id,email,password_hash,status,email_verified_at,created_at,updated_at FROM \
             users WHERE id=$1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .map(Into::into))
    }
}
