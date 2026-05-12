//! Telemetry initialisation. Sets up a layered `tracing` subscriber
//! (pretty stdout + daily-rotated JSON file) and returns a guard whose
//! `Drop` flushes the file appender. Callers must keep it alive for the
//! lifetime of the process.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

/// Subscriber config — language-agnostic so this crate doesn't have to
/// reach into `ai-llm-service::config`.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Directory the daily-rotated file appender writes to. Created if
    /// it doesn't exist.
    pub log_dir: PathBuf,
    /// File-name prefix; appender adds the `.log` extension + daily date.
    pub log_file_prefix: String,
    /// Fallback log level when `RUST_LOG` is unset (e.g. `"info"`).
    pub log_level: String,
}

/// RAII guard that flushes the file appender on drop. **Must** outlive
/// the program — once dropped, the background writer thread terminates
/// and queued log lines may be lost.
pub struct TelemetryGuard {
    _file_writer: WorkerGuard,
}

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("failed to create log dir {path}: {source}")]
    LogDir {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// Install the global tracing subscriber. Sprint 2 will extend this to
/// optionally layer an OTLP exporter on top.
pub fn init_telemetry(cfg: &TelemetryConfig) -> Result<TelemetryGuard, TelemetryError> {
    ensure_dir(&cfg.log_dir)?;

    let file_appender = rolling::daily(&cfg.log_dir, format!("{}.log", cfg.log_file_prefix));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log_level));

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

    Ok(TelemetryGuard {
        _file_writer: guard,
    })
}

fn ensure_dir(path: &Path) -> Result<(), TelemetryError> {
    std::fs::create_dir_all(path).map_err(|source| TelemetryError::LogDir {
        path: path.display().to_string(),
        source,
    })
}
