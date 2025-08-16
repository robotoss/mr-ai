//! Retrieval helpers for vector search and RAG context building.
//!
//! This module encapsulates two core functionalities:
//! 1. Low-level vector search through Qdrant.
//! 2. Building retrieval-augmented generation (RAG) context
//!    by embedding queries and fetching top-K candidates.

use std::sync::Arc;

use crate::config::RagConfig;
use crate::embed::EmbeddingsProvider;
use crate::errors::RagError;
use crate::qdrant_facade::QdrantFacade;
use crate::record::{RagHit, RagQuery};
use qdrant_client::qdrant::Filter;
use tracing::{debug, info, warn};

/// Executes a low-level vector search against the QdrantFacade.
///
/// This function is a thin wrapper around `QdrantFacade::search`
/// that handles request forwarding and error propagation.
///
/// # Arguments
/// * `client` - The Qdrant facade instance used for search.
/// * `query_vector` - The embedding vector to search with.
/// * `top_k` - Maximum number of search results to return.
/// * `filter` - Optional filtering expression applied server-side.
/// * `with_payload` - Whether to return payload (metadata) for results.
/// * `exact` - If `true`, disables approximate optimizations.
///
/// # Returns
/// Vector of `(score, payload-json)` pairs.
pub async fn search_by_vector(
    _cfg: &RagConfig,
    client: &QdrantFacade,
    query_vector: Vec<f32>,
    top_k: u64,
    filter: Option<Filter>,
    with_payload: bool,
    exact: bool,
) -> Result<Vec<(f32, serde_json::Value)>, RagError> {
    debug!(
        "search_by_vector: top_k={}, with_payload={}, exact={}",
        top_k, with_payload, exact
    );
    let res = client
        .search(query_vector, top_k, filter, with_payload, exact)
        .await?;
    Ok(res)
}

/// Builds retrieval-augmented generation (RAG) context for a free-text query.
///
/// Workflow:
/// 1. **Embed the query text** via the given embeddings provider.
/// 2. **Search in Qdrant** for top-K most relevant items.
/// 3. **Convert search results into `RagHit` records**, expecting
///    payloads to contain `"text"` and optional `"source"`.
///
/// # Arguments
/// * `cfg` - Global RAG configuration (e.g. exact search mode).
/// * `client` - Qdrant search client.
/// * `query` - The query wrapper containing text and options.
/// * `provider` - Shared embeddings provider wrapped in `Arc`.
///
/// # Returns
/// A vector of `RagHit` records sorted by score (highest relevance first).
pub async fn rag_context(
    cfg: &RagConfig,
    client: &QdrantFacade,
    query: RagQuery<'_>,
    provider: &dyn EmbeddingsProvider,
) -> Result<Vec<RagHit>, RagError> {
    info!("rag_context: embedding query, top_k={}", query.top_k);

    // Clone the Arc so it can safely move into the blocking task.
    // This prevents lifetime issues with references crossing async boundaries.
    let q = query.text.to_string();
    let qvec = provider.embed(&q).await?;

    debug!("rag_context: query embedding length={}", qvec.len());

    // Convert optional filter into Qdrant-compatible format.
    let qfilter = query.filter.as_ref().map(crate::filters::to_qdrant_filter);

    // Perform vector search in Qdrant.
    let hits = client
        .search(qvec, query.top_k, qfilter, true, cfg.exact_search)
        .await?;

    if hits.is_empty() {
        warn!("rag_context: no hits found for query");
    }

    // Convert Qdrant results into RagHit structures.
    let mut out = Vec::with_capacity(hits.len());
    for (score, payload) in hits.into_iter() {
        let (text, source) = extract_text_source(&payload);
        out.push(RagHit {
            score,
            text,
            source,
            raw_payload: payload,
        });
    }

    info!("rag_context: {} hits processed", out.len());
    Ok(out)
}

/// Helper: extracts `(text, source)` fields from Qdrant payload.
/// Falls back to empty string and `None` if missing.
fn extract_text_source(payload: &serde_json::Value) -> (String, Option<String>) {
    use serde_json::Value as J;
    match payload {
        J::Object(m) => {
            let text = m
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let source = m
                .get("source")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (text, source)
        }
        _ => ("".into(), None),
    }
}
