use std::{env, sync::Arc};

mod core;
mod error_handler;
mod models;
mod routes;

use ai_llm_service::service_profiles::LlmServiceProfiles;
use axum::{
    Router,
    response::IntoResponse,
    routing::{get, post},
};
use tokio::signal;

use crate::{
    core::app_state::{AppConfig, AppState},
    error_handler::{AppError, AppResult},
    routes::{
        ask::ask_question_route::ask_question, prepare_graph_route::prepare_graph,
        prepare_qdrant_route::prepare_qdrant, sync_git::sync_git_route::sync_git_route,
        trigger_gitlab_mr::trigger_gitlab_mr_route::trigger_gitlab_mr,
        upload_project_data::upload_project_data,
    },
};

pub async fn start(svc: Arc<LlmServiceProfiles>) -> AppResult<()> {
    // Strict env read with explicit error
    let host_url = env::var("API_ADDRESS").map_err(|_| AppError::MissingEnv("API_ADDRESS"))?;

    // Strict config read (no defaults)
    let config = Arc::new(AppConfig::from_env()?);

    // Build shared state
    let shared_state = Arc::new(AppState::new(config.clone(), svc));

    // Routes
    let app = Router::new()
        .route("/sync_git", get(sync_git_route))
        .route("/prepare_graph", get(prepare_graph))
        .route("/prepare_qdrant", get(prepare_qdrant))
        .route("/ask_question", post(ask_question))
        .route("/trigger_git_mr", post(trigger_gitlab_mr)) // name agnostic at route-level if desired
        .route("/upload_project_data", post(upload_project_data))
        .fallback(handler_404) // unified 404
        .with_state(shared_state);

    // Bind & serve with graceful shutdown
    let listener = tokio::net::TcpListener::bind(&host_url)
        .await
        .map_err(AppError::Bind)?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(AppError::Server)?;

    Ok(())
}

/// Graceful shutdown on Ctrl+C.
async fn shutdown_signal() {
    if let Err(e) = signal::ctrl_c().await {
        // If even listening for Ctrl+C fails, just log to stderr.
        eprintln!("failed to listen for shutdown signal: {e}");
    }
}

/// Fallback handler for unmatched routes.
async fn handler_404() -> impl IntoResponse {
    AppError::NotFound
}
