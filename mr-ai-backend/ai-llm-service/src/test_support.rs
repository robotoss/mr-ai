//! Test fixtures for downstream crates.
//!
//! `dummy_gateway()` returns a fully-wired `LlmGateway` whose providers
//! never reach the network — every completion / embedding call returns a
//! deterministic canned response. This is the constructor every
//! integration test should use when it needs an `Arc<LlmGateway>` but
//! doesn't actually exercise the LLM.

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::ProviderKind;
use crate::errors::GatewayError;
use crate::gateway::LlmGateway;
use crate::traits::{EmbeddingProvider, HealthInfo, LlmProvider};
use crate::unified::{
    CostEstimate, EmbeddingRequest, EmbeddingResponse, TokenUsage, UnifiedRequest,
    UnifiedResponse, new_request_id,
};

/// Canned-response chat provider. The completion `content` echoes the
/// user-supplied prompt prefixed with `dummy:`; usage counters report
/// zero tokens. Reachable, immediate, deterministic.
#[derive(Debug, Default, Clone)]
pub struct DummyLlmProvider {
    pub canned: Option<String>,
}

impl DummyLlmProvider {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_response(canned: impl Into<String>) -> Self {
        Self {
            canned: Some(canned.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for DummyLlmProvider {
    async fn complete(&self, req: UnifiedRequest) -> Result<UnifiedResponse, GatewayError> {
        let prompt = req
            .messages
            .iter()
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let content = self
            .canned
            .clone()
            .unwrap_or_else(|| format!("dummy:{prompt}"));
        Ok(UnifiedResponse {
            content,
            provider: ProviderKind::Ollama,
            model: "dummy".into(),
            usage: TokenUsage::default(),
            cost: CostEstimate::default(),
            request_id: req.request_id,
            latency_ms: 0,
        })
    }

    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        Ok(HealthInfo::ok("dummy", 0))
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Ollama
    }
    fn model(&self) -> &str {
        "dummy"
    }
    fn endpoint(&self) -> &str {
        "memory://dummy"
    }
}

/// Embedding provider returning a fixed-length zero vector for every
/// input.
#[derive(Debug, Clone)]
pub struct DummyEmbeddingProvider {
    pub dim: usize,
}

impl Default for DummyEmbeddingProvider {
    fn default() -> Self {
        Self { dim: 8 }
    }
}

#[async_trait]
impl EmbeddingProvider for DummyEmbeddingProvider {
    async fn embed_batch(
        &self,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, GatewayError> {
        let vectors = req.inputs.iter().map(|_| vec![0.0_f32; self.dim]).collect();
        Ok(EmbeddingResponse {
            vectors,
            provider: ProviderKind::Ollama,
            model: "dummy-embed".into(),
            usage: TokenUsage::default(),
            cost: CostEstimate::default(),
            request_id: new_request_id(),
            latency_ms: 0,
        })
    }

    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        Ok(HealthInfo::ok("dummy-embed", 0))
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Ollama
    }
    fn model(&self) -> &str {
        "dummy-embed"
    }
    fn endpoint(&self) -> &str {
        "memory://dummy-embed"
    }
}

/// Build a `LlmGateway` whose every tier uses the in-memory dummy
/// providers above. Cheap, sync-safe, deterministic.
pub fn dummy_gateway() -> Arc<LlmGateway> {
    Arc::new(LlmGateway::with_providers(
        Arc::new(DummyLlmProvider::new()),
        Arc::new(DummyLlmProvider::new()),
        Arc::new(DummyEmbeddingProvider::default()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ModelTier;

    #[tokio::test]
    async fn dummy_gateway_completes_without_network() {
        let gw = dummy_gateway();
        let resp = gw
            .complete(ModelTier::Smart, UnifiedRequest::user_only("hello"))
            .await
            .unwrap();
        assert_eq!(resp.content, "dummy:hello");
        assert_eq!(resp.model, "dummy");
    }

    #[tokio::test]
    async fn dummy_gateway_health_all_reports_ok() {
        let gw = dummy_gateway();
        let snapshots = gw.health_all().await;
        assert!(snapshots.iter().all(|s| s.ok));
    }
}
