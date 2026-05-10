//! Telemetry / tracing initialisation.
//!
//! Sets up two layers:
//! - `fmt::layer().pretty()` over stdout for developer-friendly output;
//! - `fmt::layer().json()` over a daily-rotated file appender for
//!   machine-parseable persistence.
//!
//! Returns the `WorkerGuard` from `tracing-appender`; callers must keep it
//! alive for the lifetime of the application — once dropped, the background
//! writer thread terminates and pending log lines may be lost.

use std::io::{self, IsTerminal};
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::config::LogConfig;
use crate::errors::GatewayError;

/// Initialises the global tracing subscriber.
///
/// Returns a [`WorkerGuard`] which must be retained by the caller.
pub fn init_tracing(cfg: &LogConfig) -> Result<WorkerGuard, GatewayError> {
    ensure_dir(&cfg.dir)?;

    let file_appender = rolling::daily(&cfg.dir, format!("{}.log", cfg.file_prefix));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.level));

    let stdout_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_level(true)
        .with_ansi(io::stdout().is_terminal())
        .pretty();

    let file_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_level(true)
        .with_ansi(false)
        .with_writer(non_blocking)
        .json();

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    Ok(guard)
}

fn ensure_dir(path: &Path) -> Result<(), GatewayError> {
    if let Err(e) = std::fs::create_dir_all(path) {
        return Err(GatewayError::Health(crate::errors::HealthError::Decode(
            format!("failed to create log dir {}: {e}", path.display()),
        )));
    }
    Ok(())
}
