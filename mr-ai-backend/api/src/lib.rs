use std::{env, error::Error};

mod models;
mod routes;

use axum::{
    Router,
    routing::{get, post},
};
use tokio::signal;

use crate::routes::{
    ask::ask_question_route::ask_question, prepare_graph_route::prepare_graph,
    prepare_qdrant_route::prepare_qdrant, upload_project_data::upload_project_data,
};

pub async fn start() -> Result<(), Box<dyn Error>> {
    let host_url = env::var("API_ADDRESS").expect("API_ADDRESS must be set in environment");

    let app = Router::new()
        .route("/prepare_graph", get(prepare_graph))
        .route("/prepare_qdrant", get(prepare_qdrant))
        .route("/ask_question", post(ask_question))
        .route("/upload_project_data", post(upload_project_data));

    // Bind to address
    let listener = tokio::net::TcpListener::bind(&host_url).await.unwrap();

    // Start server with graceful shutdown on Ctrl+C
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();

    Ok(())
}

/// Returns a future that resolves when Ctrl+C is pressed
async fn shutdown_signal() {
    // Wait for the Ctrl+C signal
    signal::ctrl_c()
        .await
        .expect("Failed to listen for shutdown signal");
}
