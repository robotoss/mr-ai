//! High-level RAG facade: ingestion + retrieval over Qdrant.
//!
//! This crate provides a clean API to:
//! - Discover and ingest JSONL dumps with precomputed or on-the-fly embeddings
//! - Retrieve top‑K context (RAG) for a textual query
//!
//! The design is flat (no deep nesting) and splits responsibilities into focused modules.

mod config;
mod discovery;
mod embed;
mod errors;
mod filters;
mod ingest;
mod io_jsonl;
mod qdrant_facade;
mod record;
mod retrieve;

pub use config::{DistanceKind, RagConfig, VectorSpace};
pub use embed::{EmbeddingPolicy, EmbeddingsProvider};
pub use errors::RagError;
pub use record::{RagFilter, RagHit, RagQuery, RagRecord};

use tracing::{debug, trace};

/// High-level facade that wires configuration and Qdrant client.
///
/// This is the single entry point recommended for application code.
pub struct RagStore {
    cfg: RagConfig,
    client: qdrant_facade::QdrantFacade,
}

impl RagStore {
    /// Constructs a new store from the given configuration.
    ///
    /// # Errors
    /// Returns `RagError::Config` if the client initialization fails.
    pub fn new(cfg: RagConfig) -> Result<Self, RagError> {
        trace!("RagStore::new collection={}", cfg.collection);
        let client = qdrant_facade::QdrantFacade::new(&cfg)?;
        Ok(Self { cfg, client })
    }

    /// Ingests `rag_records.jsonl` from the latest timestamp under:
    /// `<root>/project_x/graphs_data/<YYYYMMDD_HHMMSS>/rag_records.jsonl`.
    ///
    /// Uses `EmbeddingPolicy` to resolve vectors.
    ///
    /// # Errors
    /// Returns errors on I/O, parse, vector size mismatch, or Qdrant failures.
    pub async fn ingest_latest_from(
        &self,
        root: impl AsRef<std::path::Path>,
        policy: EmbeddingPolicy<'_>,
    ) -> Result<usize, RagError> {
        debug!("RagStore::ingest_latest_from root={:?}", root.as_ref());
        ingest::ingest_latest_from(&self.cfg, root, policy, &self.client).await
    }

    /// Ingests records from an explicit JSONL path.
    ///
    /// # Errors
    /// Returns errors on I/O, parse, vector size mismatch, or Qdrant failures.
    pub async fn ingest_file(
        &self,
        jsonl_path: impl AsRef<std::path::Path>,
        policy: EmbeddingPolicy<'_>,
    ) -> Result<usize, RagError> {
        trace!("RagStore::ingest_file path={:?}", jsonl_path.as_ref());
        ingest::ingest_file(&self.cfg, jsonl_path, policy, &self.client).await
    }

    /// Performs a low-level vector search and returns `(score, payload)` tuples.
    ///
    /// # Errors
    /// Returns `RagError::Qdrant` if search fails.
    pub async fn search_by_vector(
        &self,
        query_vector: Vec<f32>,
        top_k: u64,
        filter: Option<RagFilter>,
        with_payload: bool,
    ) -> Result<Vec<(f32, serde_json::Value)>, RagError> {
        trace!("RagStore::search_by_vector top_k={top_k} with_payload={with_payload}");
        let qfilter = filter.as_ref().map(filters::to_qdrant_filter);
        retrieve::search_by_vector(
            &self.cfg,
            &self.client,
            query_vector,
            top_k,
            qfilter,
            with_payload,
            self.cfg.exact_search,
        )
        .await
    }

    /// Builds RAG context for a textual query using the provided embedding provider.
    ///
    /// # Errors
    /// Returns embedding errors or Qdrant failures.
    pub async fn rag_context(
        &self,
        query: RagQuery<'_>,
        provider: &dyn EmbeddingsProvider,
    ) -> Result<Vec<RagHit>, RagError> {
        trace!("RagStore::rag_context top_k={}", query.top_k);
        retrieve::rag_context(&self.cfg, &self.client, query, provider).await
    }
}
