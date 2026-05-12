//! OTLP exporter setup. Wired into [`crate::init_telemetry`] only when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set so dev runs without a collector
//! stay quiet.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use thiserror::Error;

/// Tunables for the OTLP exporter. All optional — only `endpoint` is
/// required, the rest default to sensible values for a local collector.
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    /// gRPC endpoint, e.g. `http://otel-collector:4317`.
    pub endpoint: String,
    /// `service.name` resource attribute. Defaults to `mr-ai-backend`.
    pub service_name: String,
}

impl OtlpConfig {
    /// Build from env. Returns `None` if `OTEL_EXPORTER_OTLP_ENDPOINT`
    /// is unset — the caller treats this as "OTLP disabled".
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;
        let endpoint = endpoint.trim().to_string();
        if endpoint.is_empty() {
            return None;
        }
        let service_name = std::env::var("OTEL_SERVICE_NAME")
            .unwrap_or_else(|_| "mr-ai-backend".to_string());
        Some(Self {
            endpoint,
            service_name,
        })
    }
}

#[derive(Debug, Error)]
pub enum OtlpError {
    #[error("failed to build OTLP exporter: {0}")]
    Exporter(String),
}

/// Install the W3C `TraceContext` propagator, build a batched OTLP
/// exporter, and return the provider + a configured tracer the caller
/// can plug into `tracing_opentelemetry::layer().with_tracer(tracer)`.
///
/// Returning the tracer instead of a pre-built layer keeps the layer's
/// generic `S` type free, so the caller composes it with whatever
/// `Layered<...>` subscriber stack they happen to be building.
pub fn build_otlp_tracer(
    cfg: &OtlpConfig,
) -> Result<(SdkTracerProvider, SdkTracer), OtlpError> {
    // W3C TraceContext: makes `traceparent` injection / extraction
    // round-trip cleanly across the worker boundary.
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&cfg.endpoint)
        .build()
        .map_err(|e| OtlpError::Exporter(e.to_string()))?;

    let resource = Resource::builder()
        .with_service_name(cfg.service_name.clone())
        .build();

    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();

    let tracer = provider.tracer(cfg.service_name.clone());
    Ok((provider, tracer))
}
