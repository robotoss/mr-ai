//! Distributed-tracing wiring on top of `tracing` + `opentelemetry`.
//!
//! Sprint 2 ships:
//! - [`otlp`] — opt-in OTLP exporter (gRPC :4317) controlled by
//!   `OTEL_EXPORTER_OTLP_ENDPOINT`. When unset, no exporter is wired —
//!   only the existing stdout + JSON-file subscriber layers run.
//! - [`propagation`] — small helpers for injecting / extracting W3C
//!   `traceparent` headers across the worker job boundary so a webhook
//!   trace stays connected to the worker span that handles it.

pub mod otlp;
pub mod propagation;
