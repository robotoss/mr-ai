use std::error::Error;

use api;
use tracing_subscriber::{EnvFilter, FmtSubscriber, fmt, layer::SubscriberExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Load environment variables from .env file.
    // Fails if .env file not found, not readable or invalid.
    dotenvy::dotenv()?;

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info,codegraph_prep=info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false));

    let subscriber = FmtSubscriber::builder().finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    api::start().await?;

    Ok(())
}
