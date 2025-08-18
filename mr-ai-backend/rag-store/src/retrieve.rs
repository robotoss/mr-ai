//! Retrieval helpers for vector search and RAG context building.
//!
//! This module encapsulates two core functionalities:
//! 1. Low-level vector search through Qdrant.
//! 2. Building retrieval-augmented generation (RAG) context
//!    by embedding queries and fetching top-K candidates.

use crate::config::RagConfig;
use crate::embed::EmbeddingsProvider;
use crate::errors::RagError;
use crate::qdrant_facade::QdrantFacade;
use crate::record::{RagHit, RagQuery};
use qdrant_client::qdrant::Filter;
use tracing::{debug, info, trace, warn};

/// Executes a low-level vector search against the QdrantFacade.
///
/// This is a thin wrapper around `QdrantFacade::search`.
/// Adds logging and error propagation.
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
    trace!("search_by_vector: query_vector_dim={}", query_vector.len());

    let res = client
        .search(query_vector, top_k, filter, with_payload, exact)
        .await?;

    debug!("search_by_vector: got {} hits", res.len());
    Ok(res)
}

/// Builds retrieval-augmented generation (RAG) context for a free-text query.
///
/// Workflow:
/// 1. Embed the query text via the given embeddings provider.
/// 2. Search in Qdrant for top-K most relevant items.
/// 3. Convert search results into `RagHit` records, extracting canonical fields.
///
/// # Arguments
/// * `cfg` - Global RAG configuration (e.g. exact search mode).
/// * `client` - Qdrant search client.
/// * `query` - The query wrapper containing text and options.
/// * `provider` - Shared embeddings provider.
///
/// # Returns
/// Vector of `RagHit` records sorted by score (highest relevance first).
pub async fn rag_context(
    cfg: &RagConfig,
    client: &QdrantFacade,
    query: RagQuery<'_>,
    provider: &dyn EmbeddingsProvider,
) -> Result<Vec<RagHit>, RagError> {
    info!("rag_context: embedding query text, top_k={}", query.top_k);
    trace!("rag_context: raw query text={}", query.text);

    // Embed the query
    let qvec = provider.embed(query.text).await?;
    debug!("rag_context: query embedding length={}", qvec.len());

    // Build optional Qdrant filter
    let qfilter = query.filter.as_ref().map(crate::filters::to_qdrant_filter);
    if qfilter.is_some() {
        trace!("rag_context: using custom filter");
    }

    // Perform vector search
    let hits = client
        .search(qvec, query.top_k, qfilter, true, cfg.exact_search)
        .await?;

    if hits.is_empty() {
        warn!("rag_context: no hits found for query");
    } else {
        info!("rag_context: {} hits retrieved", hits.len());
    }

    // Convert Qdrant results into structured RagHit records
    let mut out = Vec::with_capacity(hits.len());
    for (score, payload) in hits.into_iter() {
        let mut hit = extract_payload(&payload);
        hit.score = score;
        debug!(
            "rag_context: hit score={:.3}, text_len={}, source={:?}, kind={:?}",
            hit.score,
            hit.text.len(),
            hit.source,
            hit.kind
        );
        out.push(hit);
    }

    info!("rag_context: {} hits processed", out.len());
    Ok(out)
}

/// Helper: extract all canonical fields from Qdrant payload into `RagHit`.
///
/// Expected payload format (see ingestion):
/// ```json
/// {
///   "text": "...",
///   "source": "...",
///   "language": "dart",
///   "kind": "Class",
///   "tags": ["class","ui","widget"],
///   "fqn": "BaseHomePage",
///   "neighbors": [...],
///   "metrics": {...}
/// }
/// ```
fn extract_payload(payload: &serde_json::Value) -> RagHit {
    use serde_json::Value as J;

    let mut hit = RagHit {
        score: 0.0,
        text: "".into(),
        source: None,
        language: None,
        kind: None,
        tags: Vec::new(),
        fqn: None,
        neighbors: Vec::new(),
        metrics: None,
        raw_payload: payload.clone(),
        snippet: None,
    };

    if let J::Object(m) = payload {
        // Required fields
        hit.text = m
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        hit.source = m
            .get("source")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Optional metadata
        hit.language = m
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        hit.kind = m
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        hit.fqn = m.get("fqn").and_then(|v| v.as_str()).map(|s| s.to_string());

        hit.snippet = m
            .get("snippet")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(tags) = m.get("tags").and_then(|v| v.as_array()) {
            hit.tags = tags
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
        }

        if let Some(neigh) = m.get("neighbors").and_then(|v| v.as_array()) {
            hit.neighbors = neigh.clone();
        }

        if let Some(metrics) = m.get("metrics") {
            hit.metrics = Some(metrics.clone());
        }
    }

    hit
}
