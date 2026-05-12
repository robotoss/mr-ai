//! Configuration layer: reads runtime settings from environment variables
//! and exposes strongly typed configs for embeddings, Qdrant, and search.

use serde::{Deserialize, Serialize};

use crate::errors::rag_base_error::RagBaseError;

/// Distance metric supported by Qdrant for primary vector space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum DistanceMetric {
    Cosine,
    Dot,
    Euclid,
}

impl DistanceMetric {
    /// Parse from env string (case-insensitive). Defaults to Cosine.
    pub fn from_env(s: Option<String>) -> Self {
        match s
            .unwrap_or_else(|| "Cosine".to_string())
            .to_lowercase()
            .as_str()
        {
            "cosine" => DistanceMetric::Cosine,
            "dot" | "dotproduct" => DistanceMetric::Dot,
            "euclid" | "l2" => DistanceMetric::Euclid,
            _ => DistanceMetric::Cosine,
        }
    }
}

/// Embedding configuration consumed by `rag-base`.
///
/// Only `dim` is read by this crate today — model identity and concurrency
/// live on the LLM Gateway (`ai-llm-service`). `dim` is retained as a
/// strict invariant: every vector returned by the gateway is validated
/// against it before upsert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Embedding vector dimensionality (e.g., 1024 for bge-m3).
    pub dim: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self { dim: 1024 }
    }
}

/// Qdrant connectivity and collection parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantConfig {
    /// gRPC URL for Qdrant (e.g., "http://localhost:6334").
    pub url: String,
    /// Target collection name to (re)create (e.g., "mr_ai_code").
    pub collection: String,
    /// Vector distance metric (Cosine by default).
    pub distance: DistanceMetric,
    /// Batch size for upserts (vectors + payloads).
    pub batch_size: usize,
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:6334".to_string(),
            collection: "mr_ai_code".to_string(),
            distance: DistanceMetric::Cosine,
            batch_size: 256,
        }
    }
}

/// Search behavior knobs (top-k, thresholds, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Disable RAG completely.
    pub disabled: bool,
    /// Default top-k results to return.
    pub top_k: usize,
    /// Optional minimum score threshold for results (0.0..=1.0). Default 0.0 (soft).
    pub min_score: Option<f32>,
    /// Optional “take per target” cap when aggregating by target (not enforced here).
    pub take_per_target: Option<usize>,
    /// Optional memoization capacity for in-process caching.
    pub memo_cap: Option<usize>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            top_k: 20,
            // IMPORTANT: soft threshold for hybrid, otherwise exact text may be filtered out.
            min_score: Some(0.0),
            take_per_target: Some(3),
            memo_cap: Some(64),
        }
    }
}

/// Snippet clamping boundaries for payload & embedding inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkClampConfig {
    /// Max chars for embedding text (large budget).
    pub embed_max_chars: usize,
    /// Max lines for embedding text.
    pub embed_max_lines: usize,
    /// Max chars for preview snippet in payload (small budget).
    pub preview_max_chars: usize,
    /// Max lines for preview snippet in payload.
    pub preview_max_lines: usize,
    /// Ignore ultra-short chunks below this many characters (applies to both).
    pub min_chars: usize,
}

impl Default for ChunkClampConfig {
    fn default() -> Self {
        Self {
            embed_max_chars: 1200, // sensible default
            embed_max_lines: 80,
            preview_max_chars: 320,
            preview_max_lines: 50,
            min_chars: 16,
        }
    }
}

/// Top-level runtime configuration for the RAG module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagConfig {
    /// Logical project name (used in logs only since S5 multi-tenancy
    /// moved real scoping onto Qdrant payload filters).
    pub project_name: String,
    /// Embeddings backend configuration.
    pub embedding: EmbeddingConfig,
    /// Qdrant connectivity & collection settings.
    pub qdrant: QdrantConfig,
    /// Search behavior settings.
    pub search: SearchConfig,
    /// Snippet clamping bounds.
    pub clamp: ChunkClampConfig,
}

impl RagConfig {
    /// Build configuration from environment variables and an optional project name.
    ///
    /// Environment variables used:
    /// - `QDRANT_URL` (default: "http://localhost:6334")
    /// - `QDRANT_COLLECTION` (default: "mr_ai_code")
    /// - `QDRANT_DISTANCE` (values: "Cosine" | "Dot" | "Euclid"; default: "Cosine")
    /// - `QDRANT_BATCH_SIZE` (default: 256)
    /// - `EMBEDDING_DIM` (default: 1024) — must match the gateway's embedding model
    /// - `RAG_DISABLE` (default: false)
    /// - `RAG_TOP_K` (default: 20)
    /// - `RAG_MIN_SCORE` (default: 0.0)
    /// - `RAG_TAKE_PER_TARGET` (optional)
    /// - `RAG_MEMO_CAP` (optional)
    /// - `CLAMP_PREVIEW_MAX_CHARS` (default: 320; fallback to CHUNK_MAX_CHARS)
    /// - `CLAMP_EMBED_MAX_CHARS` (default: 1200; fallback to CHUNK_MAX_CHARS)
    /// - `CLAMP_PREVIEW_MAX_LINES` (default: 50)
    /// - `CLAMP_EMBED_MAX_LINES` (default: 80)
    /// - `CHUNK_MIN_CHARS` (default: 16)
    ///
    /// The embedding **model** and **endpoint** are owned by the LLM Gateway
    /// (`ai-llm-service`) — they are not read here. `project_name` is the
    /// single-project slug captured at boot from `projects.toml`; callers
    /// that do not have one may pass `None` and accept the placeholder
    /// "default".
    pub fn from_env(project_name: Option<&str>) -> Result<Self, RagBaseError> {
        let name = project_name
            .map(|s| s.to_string())
            .unwrap_or_else(|| "default".to_string());

        // Embedding — only `dim` is consumed locally (used to validate
        // vectors returned by the gateway and to size the Qdrant collection).
        let embedding = EmbeddingConfig {
            dim: read_usize_env("EMBEDDING_DIM").unwrap_or(1024),
        };

        // Qdrant
        let qdrant = QdrantConfig {
            url: std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6334".into()),
            collection: std::env::var("QDRANT_COLLECTION").unwrap_or_else(|_| "mr_ai_code".into()),
            distance: DistanceMetric::from_env(std::env::var("QDRANT_DISTANCE").ok()),
            batch_size: read_usize_env("QDRANT_BATCH_SIZE").unwrap_or(256),
        };

        // Search
        let search = SearchConfig {
            disabled: read_bool_env("RAG_DISABLE").unwrap_or(false),
            top_k: read_usize_env("RAG_TOP_K").unwrap_or(20),
            min_score: Some(read_f32_env("RAG_MIN_SCORE").unwrap_or(0.0)),
            take_per_target: read_usize_env("RAG_TAKE_PER_TARGET").ok(),
            memo_cap: read_usize_env("RAG_MEMO_CAP").ok(),
        };

        // Clamp
        let clamp = {
            let preview_max_chars = read_usize_env("CLAMP_PREVIEW_MAX_CHARS")
                .or_else(|_| read_usize_env("CHUNK_MAX_CHARS"))
                .unwrap_or(320);

            let embed_max_chars = read_usize_env("CLAMP_EMBED_MAX_CHARS")
                .or_else(|_| read_usize_env("CHUNK_MAX_CHARS"))
                .unwrap_or(1200);

            ChunkClampConfig {
                // Embedding budgets
                embed_max_chars,
                embed_max_lines: read_usize_env("CLAMP_EMBED_MAX_LINES").unwrap_or(80),

                // Preview budgets (payload)
                preview_max_chars,
                preview_max_lines: read_usize_env("CLAMP_PREVIEW_MAX_LINES").unwrap_or(50),

                // Minimum useful chunk size
                min_chars: read_usize_env("CHUNK_MIN_CHARS").unwrap_or(16),
            }
        };

        // Basic validations
        if embedding.dim == 0 {
            return Err(RagBaseError::InvalidConfig(
                "EMBEDDING_DIM must be > 0".into(),
            ));
        }
        if search.top_k == 0 {
            return Err(RagBaseError::InvalidConfig("RAG_TOP_K must be > 0".into()));
        }

        Ok(Self {
            project_name: name,
            embedding,
            qdrant,
            search,
            clamp,
        })
    }
}

/// Read a `usize` from env, with error mapped to `RagBaseError`.
fn read_usize_env(key: &str) -> Result<usize, RagBaseError> {
    match std::env::var(key) {
        Ok(v) => v.parse::<usize>().map_err(|_| RagBaseError::EnvParse {
            key: key.into(),
            value: v,
        }),
        Err(_) => Err(RagBaseError::EnvMissing { key: key.into() }),
    }
}

/// Read an optional `bool` from env.
fn read_bool_env(key: &str) -> Result<bool, RagBaseError> {
    match std::env::var(key) {
        Ok(v) => v.parse::<bool>().map_err(|_| RagBaseError::EnvParse {
            key: key.into(),
            value: v,
        }),
        Err(_) => Err(RagBaseError::EnvMissing { key: key.into() }),
    }
}

/// Read an optional `f32` from env.
fn read_f32_env(key: &str) -> Result<f32, RagBaseError> {
    match std::env::var(key) {
        Ok(v) => v.parse::<f32>().map_err(|_| RagBaseError::EnvParse {
            key: key.into(),
            value: v,
        }),
        Err(_) => Err(RagBaseError::EnvMissing { key: key.into() }),
    }
}
