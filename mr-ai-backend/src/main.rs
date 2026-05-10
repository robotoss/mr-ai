use std::{error::Error, sync::Arc};

use ai_llm_service::{GatewayConfig, LlmGateway, init_tracing};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Load environment variables from .env file.
    dotenvy::dotenv()?;

    // Build typed gateway config first so we can wire logging from it.
    let gateway_cfg = GatewayConfig::from_env()?;

    // Initialise tracing: pretty stdout + JSON daily-rotated file appender.
    // The returned guard must live for the duration of the process.
    let _log_guard = init_tracing(&gateway_cfg.log)?;

    let gateway = Arc::new(LlmGateway::from_config(gateway_cfg)?);

    let statuses = gateway.health_all().await;
    for s in &statuses {
        if s.ok {
            tracing::info!(role = ?s.role, provider = %s.provider, model = %s.model, latency_ms = s.latency_ms, "{}", s.message);
        } else {
            tracing::warn!(role = ?s.role, provider = %s.provider, model = %s.model, latency_ms = s.latency_ms, "{}", s.message);
        }
    }

    api::start(gateway).await?;
    Ok(())
}
