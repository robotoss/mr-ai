use std::{env, sync::Arc};

pub mod core;
mod error_handler;
mod middleware_layer;
pub mod routes;

use ai_llm_service::LlmGateway;
use axum::{
    Router, middleware,
    response::IntoResponse,
    routing::{get, post},
};
use colored::*;
use tokio::signal; // for colorful console output

use crate::{
    core::app_state::{AppConfig, AppState},
    error_handler::{AppError, AppResult},
    middleware_layer::json_extractor::json_error_mapper,
    routes::{
        admin::{
            reindex_all_route::reindex_all_route, reindex_repo_route::reindex_repo_route,
        },
        check_mr::trigger_mr_route::trigger_mr_route,
        health::{
            dashboard::dashboard_route, detailed::detailed_route, live::live_route,
            ready::ready_route,
        },
        metrics::metrics_route::metrics_route,
        retrieve::retrieve_route::retrieve_route,
        usage::usage_route::usage_route,
        webhooks::{
            bitbucket::bitbucket_webhook_route, github::github_webhook_route,
            gitlab::gitlab_webhook_route,
        },
    },
};

pub async fn start(gateway: Arc<LlmGateway>) -> AppResult<()> {
    println!("{}", "🚀 Starting service initialization...".blue().bold());

    // Strict env read with explicit error
    let host_url = env::var("API_ADDRESS").map_err(|_| AppError::MissingEnv("API_ADDRESS"))?;
    println!("{}", format!("✅ Loaded API_ADDRESS: {host_url}").green());

    // Strict env-side config (no defaults). C4 (🅲) removed the
    // single-project invariant — tenant identity is now per-request
    // via `X-Project-Slug` header → AuthorizedScope in extensions.
    let config = Arc::new(AppConfig::from_env()?);
    println!("{}", "✅ AppConfig loaded (multi-tenant)".green());

    // projects.toml is still parsed at boot for the replication path,
    // but we no longer pin a "default" project. The file may declare
    // any number of [[project]] entries.
    let projects_cfg_path =
        env::var("PROJECTS_CONFIG").unwrap_or_else(|_| "projects.toml".into());
    let project_groups = persistence::projects_config::parse_file(&projects_cfg_path)
        .map_err(|e| AppError::ProjectsConfig(e.to_string()))?;
    println!(
        "{}",
        format!(
            "✅ projects.toml parsed ({} project group(s))",
            project_groups.len()
        )
        .green()
    );

    // Secret provider — env by default, optionally file-mounted.
    let secrets_provider = secrets::from_env();
    println!(
        "{}",
        format!(
            "✅ Secret provider initialised (backend = {})",
            secrets_provider.backend_name()
        )
        .green()
    );

    // Optional Postgres pool. When DATABASE_URL is unset and DATABASE_OPTIONAL
    // is "true" (default), the binary still boots and the legacy paths keep
    // working. Set DATABASE_OPTIONAL=false in production to fail fast.
    let optional = env::var("DATABASE_OPTIONAL")
        .ok()
        .map(|s| s.eq_ignore_ascii_case("true") || s == "1")
        .unwrap_or(true);
    let db_pool = persistence::init_optional_with_migrations(optional).await?;
    if db_pool.is_some() {
        println!(
            "{}",
            "✅ Postgres pool ready and migrations applied".green()
        );
        let cfg_path = env::var("PROJECTS_CONFIG").unwrap_or_else(|_| "projects.toml".into());
        if let Some(pool) = db_pool.as_ref() {
            match persistence::projects_config::load_and_replicate(pool, &cfg_path).await {
                Ok(n) => println!(
                    "{}",
                    format!("✅ projects.toml synced ({n} project group(s))").green()
                ),
                Err(e) => return Err(AppError::ProjectsConfig(e.to_string())),
            }
        }
    } else {
        println!(
            "{}",
            "ℹ️  Postgres disabled (no DATABASE_URL); legacy paths only"
                .yellow()
        );
    }

    // Live LLM gateway health probe (background-refreshed snapshot).
    let llm_health_interval = std::time::Duration::from_secs(
        env::var("LLM_HEALTH_REFRESH_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60),
    );
    let (llm_health_monitor, llm_health_supervisor) =
        services::llm_health::LlmHealthMonitor::start(gateway.clone(), llm_health_interval).await;
    println!("{}", "✅ LLM health monitor warmed up".green());

    // RAG / Qdrant config — captured once at boot so `/retrieve`
    // doesn't re-read ~14 env vars on every request. No project_name
    // override under multi-tenant; the collection name is global and
    // payload filters do the per-tenant scoping.
    let rag_cfg = Arc::new(
        rag_base::structs::rag_base_config::RagConfig::from_env(None)
            .map_err(|e| AppError::Config(
                crate::core::app_state::ConfigError::InvalidValue {
                    name: "RAG_CONFIG",
                    reason: e.to_string(),
                },
            ))?,
    );

    // Shared Qdrant client — one connection seeded at boot so both
    // `/retrieve` and the worker's IngestMr path reuse it.
    let qdrant_client = Arc::new(
        rag_base::vector_db::connect(&rag_cfg)
            .await
            .map_err(|e| AppError::Http {
                status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
                code: "QDRANT_CONNECT_FAILED",
                message: e.to_string(),
            })?,
    );

    // Idempotent collection bootstrap. Without this the first reindex
    // hits `scroll_repo_chunk_metas` / `upsert_batch` against a
    // non-existent collection and fails — operators previously had to
    // manually run `reset_collection`. `ensure_collection` is a no-op
    // when the collection already exists, so this is safe on every
    // boot.
    if let Err(err) = rag_base::vector_db::ensure_collection(&qdrant_client, &rag_cfg).await {
        eprintln!(
            "{}",
            format!("⚠️  Qdrant ensure_collection failed: {err} \
                     (worker will fail on first reindex)")
                .yellow()
        );
    } else {
        println!(
            "{}",
            format!("✅ Qdrant collection '{}' ready", rag_cfg.qdrant.collection).green()
        );
    }

    // Prometheus recorder — install once at boot. Failure to install
    // (e.g. recorder already set in test harness) degrades to `None`
    // so `/metrics` returns an empty body instead of poisoning startup.
    let metrics_handle = match observability::install_prometheus_recorder() {
        Ok(handle) => {
            println!("{}", "✅ Prometheus recorder installed".green());
            Some(handle)
        }
        Err(err) => {
            eprintln!(
                "{}",
                format!("⚠️  Prometheus recorder install failed: {err}").yellow()
            );
            None
        }
    };

    // Dashboard snapshotter — spawns only when persistence is on.
    // Refresh cadence is small (30s default) so an ops UI can poll
    // every second without pressuring the database.
    let dashboard_refresh_secs: u64 = env::var("DASHBOARD_REFRESH_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let dashboard_cache = if let Some(pool) = db_pool.as_ref() {
        let cache = services::dashboard_monitor::spawn_dashboard_monitor(
            services::dashboard_monitor::DashboardMonitorInputs {
                pool: pool.clone(),
                gateway: gateway.clone(),
                worker_pool_size: worker::WorkerConfig::from_env().pool_size,
                refresh_interval: std::time::Duration::from_secs(dashboard_refresh_secs),
            },
        );
        println!("{}", "✅ Dashboard monitor spawned".green());
        Some(cache)
    } else {
        None
    };

    // Build shared state
    let shared_state = Arc::new(AppState::new(
        config.clone(),
        gateway.clone(),
        secrets_provider,
        db_pool.clone(),
        llm_health_monitor,
        rag_cfg.clone(),
        metrics_handle,
        dashboard_cache,
    ));
    println!("{}", "✅ Shared state initialized".green());

    // Background worker pool. Spawned only when persistence is enabled —
    // jobs live in Postgres. Returns a handle we drain on graceful shutdown.
    let worker_pool = if let Some(pool) = db_pool.as_ref() {
        let cfg = worker::WorkerConfig::from_env();
        let registry = worker::handlers::default_registry(
            worker::handlers::DefaultRegistryConfig {
                pool: pool.clone(),
                gateway: gateway.clone(),
                qdrant: qdrant_client.clone(),
                rag_cfg: rag_cfg.clone(),
                git_api_base: config.git_api_base.clone(),
            },
        )
        .map_err(|e| AppError::Http {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            code: "WORKER_INIT_ERROR",
            message: e.to_string(),
        })?;
        let pool_handle = worker::spawn_pool(pool.clone(), registry, cfg);
        println!("{}", "✅ Worker pool spawned".green());
        Some(pool_handle)
    } else {
        None
    };

    // Operator router — anything that reads the indexed codebase or
    // triggers heavy worker jobs is gated by `X-Admin-Token` matched
    // (constant-time) against `TRIGGER_SECRET`. Webhooks have their
    // own HMAC verification, health probes stay open for k8s.
    //
    // Layered middlewares (outer-most first; axum applies bottom-up):
    //   admin_auth   — validates X-Admin-Token (401 on miss)
    //   extract_tenant — resolves X-Project-Slug → AuthorizedScope
    //                     (sprint C3; 400 on miss/unknown)
    // 401 fires before 400 so a caller missing both headers sees the
    // identity error first — typical security ergonomics.
    //
    // When Postgres is available, additionally wrap every admin call
    // in the audit middleware so request_id + route + status + latency
    // + payload sha land in `audit_log`. Audit writes are spawned on
    // detached tasks so they never block the response.
    let mut admin_router = Router::new()
        .route("/admin/reindex_repo", post(reindex_repo_route))
        .route("/admin/reindex_all", post(reindex_all_route))
        .route("/retrieve", post(retrieve_route))
        .route("/trigger_git_mr", axum::routing::post(trigger_mr_route))
        .route_layer(middleware::from_fn_with_state(
            shared_state.clone(),
            crate::middleware_layer::tenant::extract_tenant,
        ))
        .route_layer(middleware::from_fn_with_state(
            shared_state.clone(),
            crate::middleware_layer::admin_auth::admin_auth,
        ));
    if let Some(pool) = db_pool.as_ref() {
        let audit_port: observability::audit::SharedAuditPort = std::sync::Arc::new(
            crate::middleware_layer::audit_port_pg::PgAuditPort::new(pool.clone()),
        );
        let audit_state = observability::AuditMiddlewareState::new(audit_port);
        admin_router = admin_router.route_layer(middleware::from_fn_with_state(
            audit_state,
            observability::audit_layer,
        ));
        spawn_audit_cleanup(pool.clone());
        spawn_rerank_cache_cleanup(pool.clone());
        println!(
            "{}",
            "✅ Audit middleware + cleanup task + rerank_cache cleanup wired".green()
        );
    }

    let app = Router::new()
        .merge(admin_router)
        .route("/webhooks/gitlab", post(gitlab_webhook_route))
        .route("/webhooks/github", post(github_webhook_route))
        .route("/webhooks/bitbucket", post(bitbucket_webhook_route))
        .route("/health/live", get(live_route))
        .route("/health/ready", get(ready_route))
        .route("/health/detailed", get(detailed_route))
        .route("/health/dashboard", get(dashboard_route))
        .route("/metrics", get(metrics_route))
        .route("/usage", get(usage_route))
        .fallback(handler_404)
        .layer(middleware::from_fn(json_error_mapper))
        .with_state(shared_state);

    println!("{}", "🔧 Routes configured successfully".blue());

    // Bind & serve with graceful shutdown
    let listener = tokio::net::TcpListener::bind(&host_url)
        .await
        .map_err(AppError::Bind)?;

    println!(
        "{}",
        format!("🌍 Server is listening on: {host_url}")
            .green()
            .bold()
    );
    println!(
        "{}",
        "🛑 Press Ctrl+C to stop the server gracefully"
            .yellow()
            .bold()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(AppError::Server)?;

    if let Some(pool) = worker_pool {
        println!("{}", "🔧 Draining worker pool...".yellow());
        pool.shutdown().await;
        println!("{}", "✅ Worker pool drained".green());
    }

    println!("{}", "🔧 Draining LLM health monitor...".yellow());
    llm_health_supervisor.shutdown().await;
    println!("{}", "✅ LLM health monitor drained".green());

    println!("{}", "👋 Server shutdown complete".yellow().bold());
    Ok(())
}

/// Graceful shutdown on Ctrl+C.
async fn shutdown_signal() {
    if let Err(e) = signal::ctrl_c().await {
        eprintln!(
            "{}",
            format!("❌ Failed to listen for shutdown signal: {e}")
                .red()
                .bold()
        );
    } else {
        println!(
            "{}",
            "📴 Shutdown signal received, cleaning up..."
                .yellow()
                .bold()
        );
    }
}

/// Fallback handler for unmatched routes.
/// Background task that prunes `audit_log` rows older than
/// `AUDIT_RETENTION_DAYS` (default 30). Sleeps a configurable interval
/// between passes — defaults to 24h. Errors are logged but never
/// crash the task; the next tick will retry.
fn spawn_audit_cleanup(pool: sqlx::PgPool) {
    let retention_days: i64 = env::var("AUDIT_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let interval_secs: u64 = env::var("AUDIT_CLEANUP_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24 * 60 * 60);
    // Avoid contending with worker_pool startup — first tick after
    // 5 minutes lets the system settle.
    const BOOT_DELAY_SECS: u64 = 5 * 60;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(BOOT_DELAY_SECS)).await;
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            ticker.tick().await;
            let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days);
            match persistence::repos::audit::delete_expired(&pool, cutoff).await {
                Ok(n) if n > 0 => tracing::info!(
                    target = "audit.cleanup",
                    deleted = n,
                    retention_days,
                    "audit_log: pruned expired rows"
                ),
                Ok(_) => tracing::debug!(
                    target = "audit.cleanup",
                    retention_days,
                    "audit_log: nothing to prune"
                ),
                Err(err) => tracing::warn!(
                    target = "audit.cleanup",
                    error = %err,
                    "audit_log: cleanup query failed; will retry next tick"
                ),
            }
        }
    });
}

/// Background task that prunes expired `rerank_cache` rows. Same
/// shape as `spawn_audit_cleanup`: 5-minute boot delay, then every
/// `RERANK_CACHE_CLEANUP_INTERVAL_SECS` (default 24h). Cache rows
/// already carry `expires_at`; the cleanup just deletes those past
/// `now()`. Errors logged, never crash the task.
fn spawn_rerank_cache_cleanup(pool: sqlx::PgPool) {
    let interval_secs: u64 = env::var("RERANK_CACHE_CLEANUP_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24 * 60 * 60);
    const BOOT_DELAY_SECS: u64 = 5 * 60;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(BOOT_DELAY_SECS)).await;
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            ticker.tick().await;
            let cutoff = chrono::Utc::now();
            match persistence::repos::rerank_cache::delete_expired(&pool, cutoff).await {
                Ok(n) if n > 0 => tracing::info!(
                    target = "rerank_cache.cleanup",
                    deleted = n,
                    "rerank_cache: pruned expired rows"
                ),
                Ok(_) => tracing::debug!(
                    target = "rerank_cache.cleanup",
                    "rerank_cache: nothing to prune"
                ),
                Err(err) => tracing::warn!(
                    target = "rerank_cache.cleanup",
                    error = %err,
                    "rerank_cache: cleanup query failed; will retry next tick"
                ),
            }
        }
    });
}

async fn handler_404() -> impl IntoResponse {
    println!("{}", "⚠️  404 Not Found request received".red());
    AppError::NotFound
}
