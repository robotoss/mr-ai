use std::{error::Error, sync::Arc};

use ai_llm_service::{config::default_config, service_profiles::LlmServiceProfiles};
use api;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Load environment variables from .env file.
    // Fails if .env file not found, not readable or invalid.
    dotenvy::dotenv()?;

    init_tracing();

    // let filter = EnvFilter::try_from_default_env()
    //     .or_else(|_| EnvFilter::try_new("debug"))
    //     .unwrap();

    // tracing_subscriber::registry()
    //     .with(filter)
    //     .with(fmt::layer().with_target(false))
    //     .init();

    let slow = default_config::config_ollama_slow()?;
    let fast = default_config::config_ollama_fast()?;
    let embedding = default_config::config_ollama_embedding()?;

    let _svc = Arc::new(LlmServiceProfiles::new(
        slow,
        Some(fast),
        embedding,
        Some(10),
    ));

    api::start().await?;
    Ok(())
}

fn init_tracing() {
    // 1) base level for the whole app (env or fallback)
    let base = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // 2) raise/lower level ONLY for this lib
    let filter = base.add_directive(ai_llm_service::telemetry::level_directive(
        tracing::Level::DEBUG,
    ));

    // 3) compose global subscriber with the lib's own styled layer
    tracing_subscriber::registry()
        .with(filter)
        .with(ai_llm_service::telemetry::layer::<_>()) // styles apply only to ai-llm-service
        .init();
}
