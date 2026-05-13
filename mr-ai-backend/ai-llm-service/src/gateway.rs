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

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::analytics::CostEstimator;
use crate::config::pricing::PriceTable;
use crate::config::provider_kind::ProviderKind;
use crate::config::{GatewayConfig, ProviderConfig, UsageConfig};
use crate::errors::GatewayError;
use crate::health::{HealthRole, HealthSnapshot};
use crate::providers::{BedrockProvider, OllamaProvider, OpenAiProvider};
use crate::traits::{EmbeddingProvider, LlmProvider};
use crate::unified::{
    EmbeddingRequest, EmbeddingResponse, TokenUsage, UnifiedRequest, UnifiedResponse,
};
use crate::usage::{
    JsonlUsageRecorder, NoopUsageRecorder, UsageCounters, UsageKind, UsageRecord, UsageRecorder,
    UsageSnapshot, redact_secrets, truncate_preview,
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
    counters: UsageCounters,
    recorder: Arc<dyn UsageRecorder>,
    record_previews: bool,
    redact_secrets: bool,
    preview_chars: usize,
    /// Per-request USD cap (sprint 4c). `None` disables enforcement —
    /// the gateway only tracks cumulative cost for telemetry.
    cost_cap_usd: Option<f64>,
    /// Cumulative USD per `request_id`. Pruned implicitly: callers
    /// build a fresh `request_id` per logical operation, so map size
    /// grows linearly with active requests and shrinks as workers
    /// drop the handle.
    cost_tracker: dashmap::DashMap<String, f64>,
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
            .field("recorder", &self.recorder)
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

        let recorder = build_recorder(&cfg.usage);

        info!(
            fast.provider = %fast_meta.provider,
            fast.model = %fast_meta.model,
            smart.provider = %smart_meta.provider,
            smart.model = %smart_meta.model,
            embed.provider = %embedding_meta.provider,
            embed.model = %embedding_meta.model,
            usage.enabled = !cfg.usage.disabled,
            usage.path = %cfg.usage.path.display(),
            usage.previews = cfg.usage.include_prompts,
            "LlmGateway initialised"
        );

        // Cost cap is opt-in via env. `None` keeps the legacy
        // unbounded path; any positive value enables enforcement.
        let cost_cap_usd = std::env::var("LLM_MAX_COST_PER_REQUEST_USD")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0);

        Ok(Self {
            completions,
            embeddings,
            cost: CostEstimator::new(price_table),
            fast_meta,
            smart_meta,
            embedding_meta,
            counters: UsageCounters::new(),
            recorder,
            record_previews: cfg.usage.include_prompts,
            redact_secrets: cfg.usage.redact_secrets,
            preview_chars: cfg.usage.preview_chars,
            cost_cap_usd,
            cost_tracker: dashmap::DashMap::new(),
        })
    }

    /// Build a gateway from explicit provider trait objects. **Test- and
    /// fixture-only.** This bypasses `pricing.toml`, so cost analytics
    /// always report `0.0` regardless of provider — production code paths
    /// must go through [`LlmGateway::from_config`]. Recorder is hardwired
    /// to `NoopUsageRecorder`; previews are disabled.
    pub fn with_providers(
        fast: Arc<dyn crate::traits::LlmProvider>,
        smart: Arc<dyn crate::traits::LlmProvider>,
        embedding: Arc<dyn crate::traits::EmbeddingProvider>,
    ) -> Self {
        let fast_meta = ProviderMeta {
            provider: fast.provider_kind(),
            model: fast.model().to_owned(),
            endpoint: fast.endpoint().to_owned(),
        };
        let smart_meta = ProviderMeta {
            provider: smart.provider_kind(),
            model: smart.model().to_owned(),
            endpoint: smart.endpoint().to_owned(),
        };
        let embedding_meta = ProviderMeta {
            provider: embedding.provider_kind(),
            model: embedding.model().to_owned(),
            endpoint: embedding.endpoint().to_owned(),
        };

        let mut completions: HashMap<ModelTier, Arc<dyn crate::traits::LlmProvider>> =
            HashMap::new();
        completions.insert(ModelTier::Fast, fast);
        completions.insert(ModelTier::Smart, smart);
        let mut embeddings: HashMap<EmbeddingTier, Arc<dyn crate::traits::EmbeddingProvider>> =
            HashMap::new();
        embeddings.insert(EmbeddingTier::Default, embedding);

        Self {
            completions,
            embeddings,
            cost: CostEstimator::new(PriceTable::empty()),
            fast_meta,
            smart_meta,
            embedding_meta,
            counters: UsageCounters::new(),
            recorder: Arc::new(crate::usage::NoopUsageRecorder),
            record_previews: false,
            redact_secrets: true,
            preview_chars: 0,
            cost_cap_usd: None,
            cost_tracker: dashmap::DashMap::new(),
        }
    }

    /// Cheap accessor for the configured per-request budget. Returns
    /// `None` when enforcement is disabled. Mostly useful for ops
    /// dashboards and the cost-aware tests below.
    pub fn cost_cap_usd(&self) -> Option<f64> {
        self.cost_cap_usd
    }

    /// Read-only view of the cumulative cost recorded for one
    /// `request_id`. Returns `None` when no calls have landed.
    pub fn cumulative_cost(&self, request_id: &str) -> Option<f64> {
        self.cost_tracker.get(request_id).map(|v| *v)
    }

    /// Drop the per-request accumulator. Callers should invoke this
    /// once the logical operation ends so the map doesn't grow
    /// unboundedly. No-op when nothing was tracked.
    pub fn release_request(&self, request_id: &str) {
        self.cost_tracker.remove(request_id);
    }

    /// Enforce the per-request USD cap **before** an LLM call. The
    /// estimate is a deliberately coarse char-count heuristic
    /// (4 chars ≈ 1 token, OpenAI rule of thumb) — enough to reject
    /// obvious overruns without re-implementing tokenization.
    fn pre_flight_check(
        &self,
        request_id: &str,
        char_count: usize,
        tier_provider: ProviderKind,
        tier_model: &str,
    ) -> Result<(), GatewayError> {
        let Some(cap) = self.cost_cap_usd else {
            return Ok(());
        };
        let estimated_tokens = (char_count / 4) as u32;
        let usage_estimate = TokenUsage {
            prompt: estimated_tokens,
            completion: 0,
            total: estimated_tokens,
        };
        let est = self
            .cost
            .estimate(tier_provider, tier_model, usage_estimate)
            .usd;
        let cumulative = self.cumulative_cost(request_id).unwrap_or(0.0);
        if cumulative + est > cap {
            observability::counter!(
                "llm_cost_cap_exceeded_total",
                "phase" => "pre_flight",
            )
            .increment(1);
            return Err(GatewayError::CostCapExceeded {
                request_id: request_id.to_owned(),
                cumulative_usd: cumulative + est,
                cap_usd: cap,
            });
        }
        Ok(())
    }

    /// Update the per-request accumulator and trip the cap if the
    /// **actual** cost (not the estimate) crossed the budget. The
    /// current call already happened; this only blocks the next one.
    fn record_cost(&self, request_id: &str, usd: f64) -> Option<GatewayError> {
        let cumulative = {
            let mut entry = self.cost_tracker.entry(request_id.to_owned()).or_insert(0.0);
            *entry += usd.max(0.0);
            *entry
        };
        let Some(cap) = self.cost_cap_usd else {
            return None;
        };
        if cumulative > cap {
            observability::counter!(
                "llm_cost_cap_exceeded_total",
                "phase" => "post_call",
            )
            .increment(1);
            Some(GatewayError::CostCapExceeded {
                request_id: request_id.to_owned(),
                cumulative_usd: cumulative,
                cap_usd: cap,
            })
        } else {
            None
        }
    }

    /// Routes a completion to the requested tier and emits the analytics log line.
    #[tracing::instrument(name = "llm.complete", skip_all, fields(tier = ?tier))]
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
        let prompt_id_label = req.prompt_id.map(|p| p.label());
        let prompt_chars: usize = req.messages.iter().map(|m| m.content.len()).sum();
        let tier_meta = match tier {
            ModelTier::Fast => &self.fast_meta,
            ModelTier::Smart => &self.smart_meta,
        };
        // Pre-flight cost cap — char-count heuristic, never an LLM call.
        self.pre_flight_check(&request_id, prompt_chars, tier_meta.provider, &tier_meta.model)?;

        let prompt_preview = self.maybe_prompt_preview(&req);
        let mut resp = provider.complete(req).await?;
        resp.cost = self.cost.estimate(resp.provider, &resp.model, resp.usage);

        // Post-call accumulator. The current call already happened —
        // the error here blocks any **next** call on the same
        // request_id from running.
        if let Some(err) = self.record_cost(&request_id, resp.cost.usd) {
            // We still return the successful response so the caller
            // sees what was produced; subsequent gateway calls will
            // fail with the same error.
            tracing::warn!(
                target = "llm.cost_cap",
                request_id = %request_id,
                "cost cap tripped after call: {err}"
            );
        }

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

        let response_preview = if self.record_previews {
            Some(self.shape_preview(&resp.content))
        } else {
            None
        };
        let tier_lbl = tier_label(tier).to_string();
        let provider_lbl = resp.provider.to_string();
        let model_lbl = resp.model.clone();
        observability::counter!(
            observability::metrics::LLM_CALLS_TOTAL,
            "kind" => "completion",
            "tier" => tier_lbl.clone(),
            "provider" => provider_lbl.clone(),
            "model" => model_lbl.clone(),
        )
        .increment(1);
        // Cost counter tracked in micro-USD as `u64` since
        // `metrics::Counter::increment` only accepts integers; one
        // `1e-6 USD` cent is the natural minimum granularity for
        // current pricing tables. Prometheus query: divide by 1_000_000.
        let cost_micro_usd = (resp.cost.usd.max(0.0) * 1_000_000.0) as u64;
        observability::counter!(
            observability::metrics::LLM_COST_MICRO_USD_TOTAL,
            "tier" => tier_lbl.clone(),
            "provider" => provider_lbl.clone(),
            "model" => model_lbl.clone(),
        )
        .increment(cost_micro_usd);
        observability::histogram!(
            observability::metrics::LLM_LATENCY_SECONDS,
            "kind" => "completion",
            "tier" => tier_lbl.clone(),
        )
        .record(resp.latency_ms as f64 / 1000.0);

        self.observe(UsageRecord {
            timestamp: Utc::now(),
            request_id: request_id.clone(),
            kind: UsageKind::Completion,
            tier: tier_lbl,
            provider: resp.provider,
            model: resp.model.clone(),
            prompt_tokens: resp.usage.prompt,
            completion_tokens: resp.usage.completion,
            total_tokens: resp.usage.total,
            cost_usd: resp.cost.usd,
            latency_ms: resp.latency_ms,
            batch_size: None,
            prompt_preview,
            response_preview,
            prompt_id: prompt_id_label,
        });

        Ok(resp)
    }

    /// Routes an embedding request to the requested tier.
    #[tracing::instrument(name = "llm.embed_batch", skip_all, fields(tier = ?tier, batch_size = req.inputs.len()))]
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
        let prompt_preview = self.maybe_embed_preview(&req);
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

        let tier_lbl = embedding_tier_label(tier).to_string();
        let provider_lbl = resp.provider.to_string();
        let model_lbl = resp.model.clone();
        observability::counter!(
            observability::metrics::LLM_CALLS_TOTAL,
            "kind" => "embedding",
            "tier" => tier_lbl.clone(),
            "provider" => provider_lbl.clone(),
            "model" => model_lbl.clone(),
        )
        .increment(1);
        let cost_micro_usd = (resp.cost.usd.max(0.0) * 1_000_000.0) as u64;
        observability::counter!(
            observability::metrics::LLM_COST_MICRO_USD_TOTAL,
            "tier" => tier_lbl.clone(),
            "provider" => provider_lbl.clone(),
            "model" => model_lbl.clone(),
        )
        .increment(cost_micro_usd);
        observability::histogram!(
            observability::metrics::LLM_LATENCY_SECONDS,
            "kind" => "embedding",
            "tier" => tier_lbl.clone(),
        )
        .record(resp.latency_ms as f64 / 1000.0);

        self.observe(UsageRecord {
            timestamp: Utc::now(),
            request_id: request_id.clone(),
            kind: UsageKind::Embedding,
            tier: tier_lbl,
            provider: resp.provider,
            model: resp.model.clone(),
            prompt_tokens: resp.usage.prompt,
            completion_tokens: 0,
            total_tokens: resp.usage.total,
            cost_usd: resp.cost.usd,
            latency_ms: resp.latency_ms,
            batch_size: Some(batch_size),
            prompt_preview,
            response_preview: None,
            prompt_id: None,
        });

        Ok(resp)
    }

    /// Returns a snapshot of cumulative usage since process start.
    ///
    /// Cheap (one read lock + clone) and suitable for `/usage` HTTP polling.
    pub fn usage_snapshot(&self) -> UsageSnapshot {
        self.counters.snapshot()
    }

    fn observe(&self, rec: UsageRecord) {
        self.counters.observe(&rec);
        self.recorder.record(&rec);
    }

    fn maybe_prompt_preview(&self, req: &UnifiedRequest) -> Option<String> {
        if !self.record_previews {
            return None;
        }
        let joined = req
            .messages
            .iter()
            .map(|m| format!("{}: {}", m.role.as_str(), m.content))
            .collect::<Vec<_>>()
            .join("\n");
        Some(self.shape_preview(&joined))
    }

    fn maybe_embed_preview(&self, req: &EmbeddingRequest) -> Option<String> {
        if !self.record_previews {
            return None;
        }
        let joined = req.inputs.join(" | ");
        Some(self.shape_preview(&joined))
    }

    /// Apply redaction (if enabled) **before** truncation. Order matters:
    /// patterns must see the full prefix to recognise a secret; truncating
    /// first could split a token mid-string and leak the head half.
    fn shape_preview(&self, raw: &str) -> String {
        let redacted: std::borrow::Cow<'_, str> = if self.redact_secrets {
            std::borrow::Cow::Owned(redact_secrets(raw))
        } else {
            std::borrow::Cow::Borrowed(raw)
        };
        truncate_preview(&redacted, self.preview_chars)
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
        Self::from_parts_with_recorder(
            fast,
            smart,
            embedding,
            price_table,
            Arc::new(NoopUsageRecorder),
        )
    }

    /// Test-only constructor that also lets the caller plug a custom
    /// [`UsageRecorder`] (e.g. an in-memory recorder for assertions).
    #[cfg(any(test, feature = "test-util"))]
    pub fn from_parts_with_recorder(
        fast: Arc<dyn LlmProvider>,
        smart: Arc<dyn LlmProvider>,
        embedding: Arc<dyn EmbeddingProvider>,
        price_table: PriceTable,
        recorder: Arc<dyn UsageRecorder>,
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
            counters: UsageCounters::new(),
            recorder,
            record_previews: false,
            redact_secrets: true,
            preview_chars: 0,
            cost_cap_usd: None,
            cost_tracker: dashmap::DashMap::new(),
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

fn tier_label(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Fast => "fast",
        ModelTier::Smart => "smart",
    }
}

fn embedding_tier_label(tier: EmbeddingTier) -> &'static str {
    match tier {
        EmbeddingTier::Default => "default",
    }
}

fn build_recorder(cfg: &UsageConfig) -> Arc<dyn UsageRecorder> {
    if cfg.disabled {
        return Arc::new(NoopUsageRecorder);
    }
    match JsonlUsageRecorder::new(cfg.path.clone()) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            warn!(
                path = %cfg.path.display(),
                error = %e,
                "failed to initialise JsonlUsageRecorder; falling back to NoopUsageRecorder"
            );
            Arc::new(NoopUsageRecorder)
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

#[cfg(test)]
mod cost_cap_tests {
    use super::*;

    /// Build a tiny gateway with a known cap so we can exercise the
    /// pre-flight + post-call paths without hitting a network. We
    /// construct `LlmGateway` directly since the cap is the only
    /// field that matters for these tests; provider maps are left
    /// empty because none of these tests reach a `complete` call.
    fn gateway_with_cap(cap_usd: Option<f64>) -> LlmGateway {
        let meta = ProviderMeta {
            provider: ProviderKind::Ollama,
            model: "test".into(),
            endpoint: "memory://x".into(),
        };
        LlmGateway {
            completions: HashMap::new(),
            embeddings: HashMap::new(),
            cost: CostEstimator::new(PriceTable::empty()),
            fast_meta: meta.clone(),
            smart_meta: meta.clone(),
            embedding_meta: meta,
            counters: UsageCounters::new(),
            recorder: Arc::new(crate::usage::NoopUsageRecorder),
            record_previews: false,
            redact_secrets: true,
            preview_chars: 0,
            cost_cap_usd: cap_usd,
            cost_tracker: dashmap::DashMap::new(),
        }
    }

    #[test]
    fn cumulative_cost_starts_empty_and_tracks_record_cost() {
        let g = gateway_with_cap(Some(0.01));
        assert_eq!(g.cumulative_cost("r1"), None);
        let _ = g.record_cost("r1", 0.002);
        assert!((g.cumulative_cost("r1").unwrap() - 0.002).abs() < 1e-9);
        let _ = g.record_cost("r1", 0.003);
        assert!((g.cumulative_cost("r1").unwrap() - 0.005).abs() < 1e-9);
    }

    #[test]
    fn record_cost_returns_cost_cap_exceeded_when_cumulative_crosses_cap() {
        let g = gateway_with_cap(Some(0.005));
        assert!(g.record_cost("r1", 0.003).is_none());
        let err = g.record_cost("r1", 0.004);
        assert!(matches!(err, Some(GatewayError::CostCapExceeded { .. })));
    }

    #[test]
    fn record_cost_is_noop_when_cap_disabled() {
        let g = gateway_with_cap(None);
        let _ = g.record_cost("r1", 999.0);
        assert!(g.cumulative_cost("r1").is_some());
        // No error returned.
        assert!(g.record_cost("r1", 999.0).is_none());
    }

    #[test]
    fn release_request_drops_the_accumulator() {
        let g = gateway_with_cap(Some(1.0));
        let _ = g.record_cost("r1", 0.1);
        g.release_request("r1");
        assert!(g.cumulative_cost("r1").is_none());
    }
}
