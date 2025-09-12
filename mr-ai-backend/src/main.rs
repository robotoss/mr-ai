use std::{error::Error, sync::Arc};

use ai_llm_service::{config::default_config, service_profiles::LlmServiceProfiles};
use api;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Load environment variables from .env file.
    // Fails if .env file not found, not readable or invalid.
    dotenvy::dotenv()?;

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("debug"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .init();

    let slow = default_config::config_ollama_slow()?;
    let fast = default_config::config_ollama_fast()?;
    let embedding = default_config::config_ollama_embedding()?;

    let svc = Arc::new(LlmServiceProfiles::new(
        slow,
        Some(fast),
        embedding,
        Some(10),
    ));

    api::start().await?;
    Ok(())
}
