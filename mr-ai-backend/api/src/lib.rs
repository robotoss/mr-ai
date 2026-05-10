use std::{env, sync::Arc};

mod core;
mod error_handler;
mod middleware_layer;
mod routes;

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
        check_mr::trigger_mr_route::trigger_mr_route,
        project_indexer::project_indexer_route::project_indexer_route,
        rag_base::{
            search_vector_base_route::search_vector_base_route,
            vector_base_index_route::vector_base_index_route,
        },
        sync_git::sync_git_route::sync_git_route,
        usage::usage_route::usage_route,
    },
};

pub async fn start(gateway: Arc<LlmGateway>) -> AppResult<()> {
    println!("{}", "🚀 Starting service initialization...".blue().bold());

    // Strict env read with explicit error
    let host_url = env::var("API_ADDRESS").map_err(|_| AppError::MissingEnv("API_ADDRESS"))?;
    println!("{}", format!("✅ Loaded API_ADDRESS: {host_url}").green());

    // Strict config read (no defaults)
    let config = Arc::new(AppConfig::from_env()?);
    println!(
        "{}",
        "✅ AppConfig successfully loaded from environment".green()
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

    // Build shared state
    let shared_state = Arc::new(AppState::new(
        config.clone(),
        gateway,
        secrets_provider,
        db_pool,
    ));
    println!("{}", "✅ Shared state initialized".green());

    // Routes
    let app = Router::new()
        .route("/sync_git", post(sync_git_route))
        .route("/project_indexer", get(project_indexer_route))
        .route("/vector_base_index", get(vector_base_index_route))
        .route("/search_vector_base", post(search_vector_base_route))
        .route("/trigger_git_mr", axum::routing::post(trigger_mr_route))
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
async fn handler_404() -> impl IntoResponse {
    println!("{}", "⚠️  404 Not Found request received".red());
    AppError::NotFound
}
