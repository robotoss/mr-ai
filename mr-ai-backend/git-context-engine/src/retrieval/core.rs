//! Canonical retrieval function shared between the HTTP `/retrieve`
//! endpoint and the in-process `rag_layer` used by `build_two_phase_review`.
//!
//! Single pass: `embed query → vector_db::search_top_k_with_filter →
//! min_score cut`. Optional graph expansion and overlay merging are
//! HTTP-specific and intentionally live in the route handler — keeping
//! this function focused makes it a deterministic unit consumers can
//! call to enrich any context (review targets, focused hypotheses, …).

use std::sync::Arc;

use ai_llm_service::{EmbeddingRequest, EmbeddingTier, LlmGateway};
use domain::{ProjectId, RepoId};
use qdrant_client::Qdrant;
use rag_base::structs::rag_base_config::RagConfig;
use rag_base::structs::rag_store::SearchHit;
use rag_base::vector_db;
use uuid::Uuid;

use crate::errors::GitContextEngineError;

/// Inputs to [`retrieve_core`]. References (`&Qdrant`, `&RagConfig`)
/// avoid copying state captured once at boot; `query` is owned so the
/// caller can pass a freshly-built string without lifetime juggling.
pub struct RetrieveCoreInput<'a> {
    pub gateway: Arc<LlmGateway>,
    pub qdrant: &'a Qdrant,
    pub rag_cfg: &'a RagConfig,
    pub project_id: ProjectId,
    pub repo_id: Option<RepoId>,
    pub query: String,
    pub top_k: usize,
    pub min_score: f32,
    pub chunk_kinds: Option<&'a [String]>,
}

/// Run the canonical retrieval pipeline. Returns `SearchHit`s whose
/// `score >= min_score`, ordered by Qdrant (descending).
///
/// Errors:
/// - empty / whitespace query → `Ok(Vec::new())` (no-op, no LLM call).
/// - embedding gateway failure → [`GitContextEngineError::Llm`].
/// - dim mismatch between embed result and `cfg.embedding.dim` →
///   [`GitContextEngineError::Internal`] (config error surfaced eagerly
///   so callers don't get a confusing Qdrant error downstream).
/// - Qdrant search failure → [`GitContextEngineError::Internal`].
pub async fn retrieve_core(
    input: RetrieveCoreInput<'_>,
) -> Result<Vec<SearchHit>, GitContextEngineError> {
    let RetrieveCoreInput {
        gateway,
        qdrant,
        rag_cfg,
        project_id,
        repo_id,
        query,
        top_k,
        min_score,
        chunk_kinds,
    } = input;

    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let embed_resp = gateway
        .embed_batch(
            EmbeddingTier::Default,
            EmbeddingRequest::new(vec![query]),
        )
        .await
        .map_err(|e| GitContextEngineError::Llm(e.to_string()))?;

    let query_vec = embed_resp
        .vectors
        .into_iter()
        .next()
        .ok_or_else(|| GitContextEngineError::Llm("embedding gateway returned empty vector".into()))?;

    if query_vec.len() != rag_cfg.embedding.dim {
        return Err(GitContextEngineError::Internal(format!(
            "embedding dim {} != configured EMBEDDING_DIM {}",
            query_vec.len(),
            rag_cfg.embedding.dim
        )));
    }

    let project_id_str = Uuid::from(project_id).simple().to_string();
    let repo_id_str = repo_id.map(|r| Uuid::from(r).simple().to_string());

    let filter = vector_db::build_retrieve_filter(
        &project_id_str,
        repo_id_str.as_deref(),
        chunk_kinds,
    );

    let top_k = top_k.max(1);
    let hits = vector_db::search_top_k_with_filter(qdrant, rag_cfg, query_vec, filter, top_k)
        .await
        .map_err(|e| GitContextEngineError::Internal(format!("qdrant search: {e}")))?;

    Ok(hits.into_iter().filter(|h| h.score >= min_score).collect())
}
