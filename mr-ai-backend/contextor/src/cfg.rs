//! Runtime configuration loaded from environment variables.

use std::sync::Arc;

use ai_llm_service::service_profiles::LlmServiceProfiles;
use rag_store::{DistanceKind, RagConfig, RagFilter};
use serde_json::Value;

/// Config bag for the gateway. All fields have defaults via `from_env`.
#[derive(Clone, Debug)]
pub struct ContextorConfig {
    pub svc: Arc<LlmServiceProfiles>,

    // RAG retrieval knobs
    pub initial_top_k: u64,
    pub context_k: usize,
    pub mmr_lambda: f32,
    pub expand_neighbors: bool,
    pub neighbor_k: u64,
    pub score_floor: f32,
    pub max_ctx_chars: usize,

    // Optional filter applied at first retrieval
    pub initial_filter: Option<RagFilter>,

    // RagStore config (host, collection, distance, etc.)
    pub qdrant_url: String,
    pub qdrant_collection: String,
    pub rag_exact: bool,
}

impl ContextorConfig {
    /// Build from environment variables with sensible defaults.
    ///
    /// # Example
    /// ```
    /// # use contextor::ContextorError;
    /// # use contextor::cfg::ContextorConfig;
    /// let cfg = ContextorConfig::from_env();
    /// assert!(cfg.initial_top_k >= 1);
    /// ```
    pub fn new(svc: Arc<LlmServiceProfiles>) -> Self {
        let initial_filter = std::env::var("RAG_FILTER_KEY")
            .ok()
            .and_then(|k| {
                std::env::var("RAG_FILTER_VALUE")
                    .ok()
                    .map(|v| (k, as_json(v)))
            })
            .map(|(k, v)| RagFilter {
                equals: vec![(k, v)],
            });

        Self {
            svc: svc,

            initial_top_k: parse("RAG_TOP_K", 12),
            context_k: parse("CTX_K", 6usize),
            mmr_lambda: parse("MMR_LAMBDA", 0.7f32),
            expand_neighbors: env("EXPAND_NEIGHBORS", "true") == "true",
            neighbor_k: parse("NEIGHBOR_K", 6),
            score_floor: parse("SCORE_FLOOR", 0.0f32),
            max_ctx_chars: parse("MAX_CTX_CHARS", 8500usize),

            initial_filter,

            qdrant_url: env("QDRANT_URL", "http://127.0.0.1:6333"),
            qdrant_collection: env("QDRANT_COLLECTION", "code_chunks"),
            rag_exact: env("RAG_EXACT_SEARCH", "false") == "true",
        }
    }

    /// Convert to a `rag_store::RagConfig` used by `RagStore`.
    ///
    /// # Example
    /// ```
    /// # use contextor::cfg::ContextorConfig;
    /// let cfg = ContextorConfig::from_env();
    /// let rag_cfg = cfg.make_rag_config();
    /// assert_eq!(rag_cfg.collection, cfg.qdrant_collection);
    /// ```
    pub fn make_rag_config(&self) -> RagConfig {
        // Optional knobs we read from env so ContextorConfig remains compact.
        let qdrant_api_key = std::env::var("QDRANT_API_KEY").ok();

        let upsert_batch = std::env::var("QDRANT_BATCH_SIZE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(256);

        let embedding_dim = std::env::var("EMBEDDING_DIM")
            .ok()
            .and_then(|s| s.parse::<usize>().ok());

        let embedding_concurrency = std::env::var("EMBEDDING_CONCURRENCY")
            .ok()
            .and_then(|s| s.parse::<usize>().ok());

        // Distance: keep it simple here;
        let distance = DistanceKind::Cosine;

        RagConfig {
            qdrant_url: self.qdrant_url.clone(),
            qdrant_api_key,
            collection: self.qdrant_collection.clone(),
            distance,
            upsert_batch,
            exact_search: self.rag_exact,
            embedding_dim,
            embedding_concurrency,
        }
    }
}

fn env(k: &str, dflt: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| dflt.to_string())
}

fn as_json(s: String) -> Value {
    serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s))
}

fn parse<T: std::str::FromStr>(k: &str, dflt: T) -> T {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(dflt)
}
