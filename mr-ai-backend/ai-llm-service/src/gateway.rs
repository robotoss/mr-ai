//! The Universal LLM Gateway — single entry point for all outbound AI traffic.
//!
//! Holds tier-keyed maps of provider trait objects:
//! - `HashMap<ModelTier, Arc<dyn LlmProvider>>` for completion;
//! - `HashMap<EmbeddingTier, Arc<dyn EmbeddingProvider>>` for vectors.
//!
//! Tiers are independently configurable; nothing about the trait itself is
//! aware of them.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::analytics::CostEstimator;
use crate::config::pricing::PriceTable;
use crate::config::provider_kind::ProviderKind;
use crate::config::{GatewayConfig, ProviderConfig};
use crate::errors::GatewayError;
use crate::health::{HealthRole, HealthSnapshot};
use crate::providers::{BedrockProvider, OllamaProvider, OpenAiProvider};
use crate::traits::{EmbeddingProvider, LlmProvider};
use crate::unified::{
    EmbeddingRequest, EmbeddingResponse, UnifiedRequest, UnifiedResponse,
};

/// Logical completion tiers exposed to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    /// Fast / cheap tier (parsing, routing, metadata extraction).
    Fast,
    /// High-quality reasoning tier (deep code review, cross-microservice analysis).
    Smart,
}

/// Logical embedding tiers. Single tier today; reserved for future multi-model setups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingTier {
    Default,
}

/// The Universal LLM Gateway.
pub struct LlmGateway {
    completions: HashMap<ModelTier, Arc<dyn LlmProvider>>,
    embeddings: HashMap<EmbeddingTier, Arc<dyn EmbeddingProvider>>,
    cost: CostEstimator,
    fast_meta: ProviderMeta,
    smart_meta: ProviderMeta,
    embedding_meta: ProviderMeta,
}

#[derive(Debug, Clone)]
struct ProviderMeta {
    provider: ProviderKind,
    model: String,
    endpoint: String,
}

impl ProviderMeta {
    fn from_cfg(cfg: &ProviderConfig) -> Self {
        Self {
            provider: cfg.provider,
            model: cfg.model.clone(),
            endpoint: cfg.endpoint.clone(),
        }
    }
}

impl std::fmt::Debug for LlmGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmGateway")
            .field("fast", &self.fast_meta)
            .field("smart", &self.smart_meta)
            .field("embedding", &self.embedding_meta)
            .field("price_table_entries", &self.cost.table().len())
            .finish()
    }
}

impl LlmGateway {
    /// Builds a new gateway from a validated [`GatewayConfig`].
    ///
    /// Loads `pricing.toml` (if present), instantiates providers for each
    /// tier, and stores them as `Arc<dyn …>` trait objects.
    pub fn from_config(cfg: GatewayConfig) -> Result<Self, GatewayError> {
        let price_table = PriceTable::from_file(&cfg.pricing_path)?;
        info!(
            pricing_path = %cfg.pricing_path.display(),
            entries = price_table.len(),
            "PriceTable loaded"
        );

        let fast_meta = ProviderMeta::from_cfg(&cfg.fast);
        let smart_meta = ProviderMeta::from_cfg(&cfg.smart);
        let embedding_meta = ProviderMeta::from_cfg(&cfg.embedding);

        let fast = build_completion_provider(cfg.fast)?;
        let smart = build_completion_provider(cfg.smart)?;
        let embedding = build_embedding_provider(cfg.embedding)?;

        let mut completions: HashMap<ModelTier, Arc<dyn LlmProvider>> = HashMap::new();
        completions.insert(ModelTier::Fast, fast);
        completions.insert(ModelTier::Smart, smart);

        let mut embeddings: HashMap<EmbeddingTier, Arc<dyn EmbeddingProvider>> = HashMap::new();
        embeddings.insert(EmbeddingTier::Default, embedding);

        info!(
            fast.provider = %fast_meta.provider,
            fast.model = %fast_meta.model,
            smart.provider = %smart_meta.provider,
            smart.model = %smart_meta.model,
            embed.provider = %embedding_meta.provider,
            embed.model = %embedding_meta.model,
            "LlmGateway initialised"
        );

        Ok(Self {
            completions,
            embeddings,
            cost: CostEstimator::new(price_table),
            fast_meta,
            smart_meta,
            embedding_meta,
        })
    }

    /// Routes a completion to the requested tier and emits the analytics log line.
    pub async fn complete(
        &self,
        tier: ModelTier,
        req: UnifiedRequest,
    ) -> Result<UnifiedResponse, GatewayError> {
        let provider = self
            .completions
            .get(&tier)
            .ok_or(GatewayError::ProviderNotConfigured(tier))?;

        let request_id = req.request_id.clone();
        let mut resp = provider.complete(req).await?;
        resp.cost = self.cost.estimate(resp.provider, &resp.model, resp.usage);

        info!(
            request_id = %request_id,
            tier = ?tier,
            provider = %resp.provider,
            model = %resp.model,
            prompt_tokens = resp.usage.prompt,
            completion_tokens = resp.usage.completion,
            total_tokens = resp.usage.total,
            cost_usd = resp.cost.usd,
            latency_ms = resp.latency_ms,
            "completion ok"
        );

        Ok(resp)
    }

    /// Routes an embedding request to the requested tier.
    pub async fn embed_batch(
        &self,
        tier: EmbeddingTier,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, GatewayError> {
        let provider = self
            .embeddings
            .get(&tier)
            .ok_or(GatewayError::ProviderNotConfigured(ModelTier::Fast))?;

        let request_id = req.request_id.clone();
        let batch_size = req.inputs.len();
        let mut resp = provider.embed_batch(req).await?;
        resp.cost = self.cost.estimate(resp.provider, &resp.model, resp.usage);

        info!(
            request_id = %request_id,
            tier = ?tier,
            provider = %resp.provider,
            model = %resp.model,
            batch_size,
            prompt_tokens = resp.usage.prompt,
            cost_usd = resp.cost.usd,
            latency_ms = resp.latency_ms,
            "embedding ok"
        );

        Ok(resp)
    }

    /// Probes every configured provider, returning a snapshot per tier.
    pub async fn health_all(&self) -> Vec<HealthSnapshot> {
        let mut out = Vec::with_capacity(3);

        out.push(probe_completion(self, ModelTier::Fast, &self.fast_meta, HealthRole::Fast).await);
        out.push(
            probe_completion(self, ModelTier::Smart, &self.smart_meta, HealthRole::Smart).await,
        );
        out.push(probe_embedding(self, EmbeddingTier::Default, &self.embedding_meta).await);

        out
    }

    pub fn fast_provider_kind(&self) -> ProviderKind {
        self.fast_meta.provider
    }
    pub fn smart_provider_kind(&self) -> ProviderKind {
        self.smart_meta.provider
    }
    pub fn embedding_provider_kind(&self) -> ProviderKind {
        self.embedding_meta.provider
    }

    /// Test-only constructor that injects pre-built provider trait objects.
    #[cfg(any(test, feature = "test-util"))]
    pub fn from_parts(
        fast: Arc<dyn LlmProvider>,
        smart: Arc<dyn LlmProvider>,
        embedding: Arc<dyn EmbeddingProvider>,
        price_table: PriceTable,
    ) -> Self {
        let fast_meta = ProviderMeta {
            provider: fast.provider_kind(),
            model: fast.model().to_string(),
            endpoint: fast.endpoint().to_string(),
        };
        let smart_meta = ProviderMeta {
            provider: smart.provider_kind(),
            model: smart.model().to_string(),
            endpoint: smart.endpoint().to_string(),
        };
        let embedding_meta = ProviderMeta {
            provider: embedding.provider_kind(),
            model: embedding.model().to_string(),
            endpoint: embedding.endpoint().to_string(),
        };

        let mut completions: HashMap<ModelTier, Arc<dyn LlmProvider>> = HashMap::new();
        completions.insert(ModelTier::Fast, fast);
        completions.insert(ModelTier::Smart, smart);

        let mut embeddings: HashMap<EmbeddingTier, Arc<dyn EmbeddingProvider>> = HashMap::new();
        embeddings.insert(EmbeddingTier::Default, embedding);

        Self {
            completions,
            embeddings,
            cost: CostEstimator::new(price_table),
            fast_meta,
            smart_meta,
            embedding_meta,
        }
    }
}

async fn probe_completion(
    gw: &LlmGateway,
    tier: ModelTier,
    meta: &ProviderMeta,
    role: HealthRole,
) -> HealthSnapshot {
    let Some(provider) = gw.completions.get(&tier) else {
        return HealthSnapshot::fail(role, meta.provider, &meta.model, &meta.endpoint, "no provider");
    };
    match provider.health_check().await {
        Ok(info) => HealthSnapshot::from_info(role, meta.provider, &meta.model, &meta.endpoint, info),
        Err(e) => {
            warn!(role = ?role, error = %e, "completion health probe failed");
            HealthSnapshot::fail(role, meta.provider, &meta.model, &meta.endpoint, e.to_string())
        }
    }
}

async fn probe_embedding(
    gw: &LlmGateway,
    tier: EmbeddingTier,
    meta: &ProviderMeta,
) -> HealthSnapshot {
    let role = HealthRole::Embedding;
    let Some(provider) = gw.embeddings.get(&tier) else {
        return HealthSnapshot::fail(role, meta.provider, &meta.model, &meta.endpoint, "no provider");
    };
    match provider.health_check().await {
        Ok(info) => HealthSnapshot::from_info(role, meta.provider, &meta.model, &meta.endpoint, info),
        Err(e) => {
            warn!(role = ?role, error = %e, "embedding health probe failed");
            HealthSnapshot::fail(role, meta.provider, &meta.model, &meta.endpoint, e.to_string())
        }
    }
}

fn build_completion_provider(cfg: ProviderConfig) -> Result<Arc<dyn LlmProvider>, GatewayError> {
    match cfg.provider {
        ProviderKind::Ollama => Ok(Arc::new(OllamaProvider::new(cfg)?) as Arc<dyn LlmProvider>),
        ProviderKind::OpenAI => Ok(Arc::new(OpenAiProvider::new(cfg)?) as Arc<dyn LlmProvider>),
        ProviderKind::Bedrock => Ok(Arc::new(BedrockProvider::new(cfg)?) as Arc<dyn LlmProvider>),
    }
}

fn build_embedding_provider(
    cfg: ProviderConfig,
) -> Result<Arc<dyn EmbeddingProvider>, GatewayError> {
    match cfg.provider {
        ProviderKind::Ollama => {
            Ok(Arc::new(OllamaProvider::new(cfg)?) as Arc<dyn EmbeddingProvider>)
        }
        ProviderKind::OpenAI => {
            Ok(Arc::new(OpenAiProvider::new(cfg)?) as Arc<dyn EmbeddingProvider>)
        }
        ProviderKind::Bedrock => {
            Ok(Arc::new(BedrockProvider::new(cfg)?) as Arc<dyn EmbeddingProvider>)
        }
    }
}
