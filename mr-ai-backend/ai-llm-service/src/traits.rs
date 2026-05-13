//! Provider-agnostic traits — the central abstraction of the LLM Gateway.
//!
//! Two traits, separated by capability. A provider may implement one or both:
//! - [`LlmProvider`] for chat completion.
//! - [`EmbeddingProvider`] for batch embedding.

use async_trait::async_trait;

use crate::config::ProviderKind;
use crate::errors::GatewayError;
use crate::unified::{EmbeddingRequest, EmbeddingResponse, UnifiedRequest, UnifiedResponse};

/// Chat-completion capability.
#[async_trait]
pub trait LlmProvider: Send + Sync + std::fmt::Debug {
    /// Performs a single, non-streaming completion.
    async fn complete(&self, req: UnifiedRequest) -> Result<UnifiedResponse, GatewayError>;

    /// Verifies that the provider endpoint and configured model are reachable.
    async fn health_check(&self) -> Result<HealthInfo, GatewayError>;

    fn provider_kind(&self) -> ProviderKind;
    fn model(&self) -> &str;
    fn endpoint(&self) -> &str;
}

/// Batch embedding capability.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync + std::fmt::Debug {
    /// Embeds a batch of inputs. Returned vectors are aligned with `req.inputs`.
    async fn embed_batch(
        &self,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, GatewayError>;

    /// Verifies that the provider endpoint and configured embedding model are reachable.
    async fn health_check(&self) -> Result<HealthInfo, GatewayError>;

    fn provider_kind(&self) -> ProviderKind;
    fn model(&self) -> &str;
    fn endpoint(&self) -> &str;
}

/// Lightweight health-probe summary returned by trait implementations.
#[derive(Debug, Clone)]
pub struct HealthInfo {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
}

impl HealthInfo {
    pub fn ok(message: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            ok: true,
            message: message.into(),
            latency_ms,
        }
    }
    pub fn fail(message: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            ok: false,
            message: message.into(),
            latency_ms,
        }
    }
}
