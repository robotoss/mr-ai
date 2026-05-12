//! Observability layer for mr-ai-backend.
//!
//! Single concern: glue together everything the runtime needs to be
//! observable in production. Today (sprint 1) that means a tracing
//! subscriber + a Prometheus recorder behind a `MetricsHandle`. Later
//! sprints add OTLP export, W3C trace propagation, and the audit
//! middleware.
//!
//! The crate is intentionally framework-light: the tracing subscriber
//! and the Prometheus recorder are installed once via [`init_telemetry`];
//! call sites use the macros from the `metrics` and `tracing` crates
//! directly. The wrapper types here exist only to (a) keep the
//! `tracing-appender` `WorkerGuard` alive for the program's lifetime and
//! (b) expose a `render()` for the `/metrics` HTTP handler.

mod init;
pub mod metrics;
pub mod tracing;

pub use init::{init_telemetry, TelemetryConfig, TelemetryError, TelemetryGuard};
pub use metrics::{install_prometheus_recorder, MetricsHandle};
pub use tracing::propagation::{inject_into_payload, set_parent_from_payload};

/// Re-export the upstream `metrics` macros so downstream crates can
/// `use observability::{counter, histogram, gauge};` without each crate
/// having to add a direct `metrics = "0.24"` dependency. The single
/// re-export point also keeps the recorder version in sync — the
/// `metrics-exporter-prometheus` recorder we install only sees calls
/// from this exact `metrics` crate version.
pub use ::metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
