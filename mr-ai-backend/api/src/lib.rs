use std::{env, error::Error};

mod routes;

use axum::{Router, routing::get};
use tokio::signal;

use crate::routes::{learn_code_route::learn_code, upload_project_data::upload_project_data};

pub async fn start() -> Result<(), Box<dyn Error>> {
    let host_url = env::var("API_ADDRESS").expect("API_ADDRESS must be set in environment");

    let app = Router::new()
        .route("/learn_code", get(learn_code))
        .route("/upload_project_data", get(upload_project_data));

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
