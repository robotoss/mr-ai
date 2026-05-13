//! Telemetry initialisation. Sets up a layered `tracing` subscriber
//! (pretty stdout + daily-rotated JSON file + optional OTLP) and
//! returns a guard whose `Drop` flushes the file appender and shuts
//! down the OTel tracer provider. Callers must keep the guard alive
//! for the lifetime of the process.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use opentelemetry_sdk::trace::SdkTracerProvider;
use thiserror::Error;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

use crate::tracing::otlp::{build_otlp_tracer, OtlpConfig, OtlpError};

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

/// RAII guard. **Must** outlive the program — once dropped:
/// - the `WorkerGuard` from `tracing-appender` flushes the JSON file
///   appender and the background writer thread exits;
/// - the `SdkTracerProvider` (when OTLP was wired) calls `shutdown()`,
///   forcing the batch exporter to flush pending spans.
pub struct TelemetryGuard {
    _file_writer: WorkerGuard,
    otel_provider: Option<SdkTracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.otel_provider.take() {
            // Best effort — shutdown errors are logged by the SDK; we
            // don't want a guard drop to panic during process teardown.
            let _ = provider.shutdown();
        }
    }
}

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("failed to create log dir {path}: {source}")]
    LogDir {
        path: String,
        #[source]
        source: io::Error,
    },

    #[error("OTLP exporter init failed: {0}")]
    Otlp(#[from] OtlpError),
}

/// Install the global tracing subscriber. When
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is set, additionally fans spans into
/// an OTLP gRPC exporter and wires the W3C `TraceContext` propagator.
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

    // OTLP layer is optional. The OpenTelemetry tracing layer is generic
    // over the inner subscriber, so we have to compose the full layered
    // stack inside the branch that knows whether the OTel tracer exists.
    let otel_provider = match OtlpConfig::from_env() {
        Some(cfg) => {
            let (provider, tracer) = build_otlp_tracer(&cfg)?;
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            tracing_subscriber::registry()
                .with(env_filter)
                .with(stdout_layer)
                .with(file_layer)
                .with(otel_layer)
                .init();
            Some(provider)
        }
        None => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(stdout_layer)
                .with(file_layer)
                .init();
            None
        }
    };

    Ok(TelemetryGuard {
        _file_writer: guard,
        otel_provider,
    })
}

fn ensure_dir(path: &Path) -> Result<(), TelemetryError> {
    std::fs::create_dir_all(path).map_err(|source| TelemetryError::LogDir {
        path: path.display().to_string(),
        source,
    })
}
