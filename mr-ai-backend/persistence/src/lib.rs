//! Postgres persistence layer for mr-ai-backend.
//!
//! Owns the connection pool, embedded migrations, and a small repository
//! surface used by the api / worker crates. Designed to be optional at boot:
//! when `DATABASE_URL` is unset and `DATABASE_OPTIONAL=true` the binary still
//! starts and runs the legacy paths.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, migrate::Migrator};
use thiserror::Error;
use tracing::{info, warn};

pub mod graph_persist;
pub mod projects_config;
pub mod repos;
pub mod tenant;

pub use tenant::with_tenant;

/// Embedded migrations (compiled from `migrations/` at build time).
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("persistence is disabled (no DATABASE_URL)")]
    Disabled,
}

pub type Result<T> = std::result::Result<T, PersistenceError>;

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

impl PoolConfig {
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("DATABASE_URL").ok()?;
        if url.trim().is_empty() {
            return None;
        }
        let max_connections = std::env::var("DATABASE_MAX_CONNECTIONS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);
        Some(Self {
            url,
            max_connections,
            acquire_timeout: Duration::from_secs(5),
        })
    }
}

pub async fn init_pool(cfg: &PoolConfig) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(cfg.acquire_timeout)
        .connect(&cfg.url)
        .await?;
    info!(target = "persistence", max_conn = cfg.max_connections, "postgres pool ready");
    Ok(pool)
}

pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    MIGRATOR.run(pool).await?;
    info!(target = "persistence", "migrations applied");
    Ok(())
}

/// Convenience: pool + migrations in one call. When `DATABASE_URL` is missing
/// and `optional` is true, returns Ok(None) so the caller can keep booting.
pub async fn init_optional_with_migrations(optional: bool) -> Result<Option<PgPool>> {
    match PoolConfig::from_env() {
        Some(cfg) => {
            let pool = init_pool(&cfg).await?;
            run_migrations(&pool).await?;
            Ok(Some(pool))
        }
        None if optional => {
            warn!(target = "persistence", "DATABASE_URL not set; running without persistence");
            Ok(None)
        }
        None => Err(PersistenceError::Disabled),
    }
}
