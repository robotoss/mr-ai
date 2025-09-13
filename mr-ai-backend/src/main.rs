use std::{error::Error, str::FromStr, sync::Arc};

use ai_llm_service::{config::default_config, service_profiles::LlmServiceProfiles};
use api;
use tracing::Level;
use tracing_subscriber::{
    EnvFilter, Layer,
    filter::{Directive, Targets},
    fmt,
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Load environment variables from .env file.
    // Fails if .env file not found, not readable or invalid.
    dotenvy::dotenv()?;

    init_tracing();

    let slow = default_config::config_ollama_slow()?;
    let fast = default_config::config_ollama_fast()?;
    let embedding = default_config::config_ollama_embedding()?;

    let svc = Arc::new(LlmServiceProfiles::new(
        slow,
        Some(fast),
        embedding,
        Some(10),
    )?);

    let statuses = svc.health_all().await?;

    println!("{:?}", statuses);

    api::start(svc).await?;
    Ok(())
}

fn init_tracing() {
    let base = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("trace"));

    let filter = base.add_directive(Directive::from_str("mr_reviewer=trace").unwrap());

    let fmt_all = fmt::layer();

    let ai_layer = ai_llm_service::telemetry::layer::<_>()
        .with_filter(Targets::new().with_target("ai_llm_service", Level::DEBUG));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_all)
        .with(ai_layer)
        .init();
}
