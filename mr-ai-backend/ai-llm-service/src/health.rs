//! Provider-agnostic health snapshots, driven by the trait.
//!
//! [`HealthSnapshot`] is JSON-serialisable and suitable for a future
//! `/healthz` HTTP endpoint.

use serde::Serialize;

use crate::config::provider_kind::ProviderKind;
use crate::traits::HealthInfo;

/// Where the snapshot was taken (which gateway slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthRole {
    Fast,
    Smart,
    Embedding,
}

/// A health snapshot suitable for `/healthz` JSON.
#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    pub role: HealthRole,
    pub provider: ProviderKind,
    pub model: String,
    pub endpoint: String,
    pub ok: bool,
    pub latency_ms: u64,
    pub message: String,
}

impl HealthSnapshot {
    pub fn from_info(
        role: HealthRole,
        provider: ProviderKind,
        model: &str,
        endpoint: &str,
        info: HealthInfo,
    ) -> Self {
        Self {
            role,
            provider,
            model: model.to_string(),
            endpoint: endpoint.to_string(),
            ok: info.ok,
            latency_ms: info.latency_ms,
            message: info.message,
        }
    }

    pub fn fail(
        role: HealthRole,
        provider: ProviderKind,
        model: &str,
        endpoint: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            role,
            provider,
            model: model.to_string(),
            endpoint: endpoint.to_string(),
            ok: false,
            latency_ms: 0,
            message: message.into(),
        }
    }
}
