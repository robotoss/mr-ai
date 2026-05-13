//! Prometheus metrics recorder + handle.
//!
//! Install the global `metrics::Recorder` once at boot via
//! [`install_prometheus_recorder`]; pass the returned [`MetricsHandle`]
//! into the api `AppState` so the `/metrics` HTTP handler can call
//! [`MetricsHandle::render`] to produce the text-formatted scrape payload.
//!
//! Call sites use the macros from the upstream `metrics` crate
//! (`counter!`, `histogram!`, `gauge!`) — no wrapper layer.

mod names;
#[cfg(test)]
mod tests;

pub use names::*;

use std::sync::Arc;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use thiserror::Error;

/// Handle to the Prometheus exposition formatter. Cheap to clone (just
/// an `Arc`); store one copy in `AppState` and reuse across requests.
#[derive(Clone)]
pub struct MetricsHandle {
    inner: Arc<PrometheusHandle>,
}

impl MetricsHandle {
    /// Render the current recorder state in Prometheus text exposition
    /// format (`text/plain; version=0.0.4`). Cheap to call repeatedly.
    pub fn render(&self) -> String {
        self.inner.render()
    }
}

impl std::fmt::Debug for MetricsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsHandle").finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("failed to install Prometheus recorder: {0}")]
    Install(String),
}

/// Install the Prometheus recorder as the global `metrics` backend.
/// Must be called once at boot; calling twice returns an error because
/// only one global recorder is allowed.
pub fn install_prometheus_recorder() -> Result<MetricsHandle, MetricsError> {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| MetricsError::Install(e.to_string()))?;
    Ok(MetricsHandle {
        inner: Arc::new(handle),
    })
}
