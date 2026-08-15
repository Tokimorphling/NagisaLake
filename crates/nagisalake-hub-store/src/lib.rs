//! PostgreSQL persistence boundary for the Nagisalake Hub.
//!
//! The store keeps durable control-plane metadata. Network sockets, worker
//! channels and other live session state intentionally stay in the Hub process.

mod accounts;
mod artifacts;
mod audit;
mod batches;
mod credentials;
mod devices;
mod gallery;
mod jobs;
mod models;
mod observability;
mod organizations;
mod quota;
mod rows;
mod sessions;
mod snapshot;
mod workflows;

pub use batches::{BatchChildJob, BatchIdempotencyInsert, BatchInsert, CommitBatchResult};
pub use models::*;
pub use observability::BacklogMetrics;
use serde::Deserialize;
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Deserialize)]
pub struct StoreConfig {
    pub url:             String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_true")]
    pub run_migrations:  bool,
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoreConfig")
            .field("url", &"[redacted]")
            .field("max_connections", &self.max_connections)
            .field("run_migrations", &self.run_migrations)
            .finish()
    }
}

const fn default_max_connections() -> u32 {
    // The Hub has several independent maintenance and dispatch loops in
    // addition to request traffic. Keep enough headroom that a slow quota
    // transaction cannot starve health checks or outbox work.
    16
}

/// New organizations start with one active job. The quota remains adjustable
/// per organization through the product API.
pub const DEFAULT_MAX_CONCURRENT_JOBS: i64 = 1;

const fn default_true() -> bool {
    true
}

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl std::fmt::Debug for PgStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PgStore")
            .field("pool", &"[redacted]")
            .finish()
    }
}

impl PgStore {
    pub async fn connect(config: &StoreConfig) -> Result<Self, StoreError> {
        if config.url.trim().is_empty() {
            return Err(StoreError::InvalidConfig("database url is empty".into()));
        }
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections.max(1))
            .acquire_timeout(Duration::from_secs(10))
            .connect(&config.url)
            .await?;
        let store = Self { pool };
        if config.run_migrations {
            store.migrate().await?;
        }
        Ok(store)
    }

    pub async fn migrate(&self) -> Result<(), StoreError> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}

fn map_unique(error: sqlx::Error) -> StoreError {
    match error {
        sqlx::Error::Database(database) if database.constraint().is_some() => {
            StoreError::Conflict(database.message().to_owned())
        }
        error => StoreError::Database(error),
    }
}

fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("database migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("store conflict: {0}")]
    Conflict(String),
    #[error("store resource not found: {0}")]
    NotFound(String),
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),
    #[error("invalid store configuration: {0}")]
    InvalidConfig(String),
}
