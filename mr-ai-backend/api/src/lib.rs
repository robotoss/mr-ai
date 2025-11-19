use std::{env, sync::Arc};

mod core;
mod error_handler;
mod middleware_layer;
mod routes;

use ai_llm_service::service_profiles::LlmServiceProfiles;
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
        ask::ask_question_route::ask_question,
        check_mr::trigger_mr_route::trigger_mr_route,
        prepare_qdrant_route::prepare_qdrant,
        project_indexer::project_indexer_route::project_indexer_route,
        rag_base::{
            search_vector_base_route::search_vector_base_route,
            vector_base_index_route::vector_base_index_route,
        },
        sync_git::sync_git_route::sync_git_route,
    },
};

pub async fn start(svc: Arc<LlmServiceProfiles>) -> AppResult<()> {
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

    // Build shared state
    let shared_state = Arc::new(AppState::new(config.clone(), svc));
    println!("{}", "✅ Shared state initialized".green());

    // Routes
    let app = Router::new()
        .route("/sync_git", post(sync_git_route))
        .route("/project_indexer", get(project_indexer_route))
        .route("/vector_base_index", get(vector_base_index_route))
        .route("/search_vector_base", post(search_vector_base_route))
        .route("/prepare_qdrant", get(prepare_qdrant))
        .route("/ask_question", post(ask_question))
        .route("/trigger_git_mr", axum::routing::post(trigger_mr_route))
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
