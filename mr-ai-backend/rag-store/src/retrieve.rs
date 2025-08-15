//! Retrieval helpers: low-level vector search and high-level RAG context.

use crate::config::RagConfig;
use crate::embed::EmbeddingsProvider;
use crate::errors::RagError;
use crate::filters::to_qdrant_filter;
use crate::qdrant_facade::QdrantFacade;
use crate::record::{RagHit, RagQuery};

use qdrant_client::qdrant::Filter;
use tracing::trace;

/// Performs a low-level similarity search given a ready query vector.
///
/// # Errors
/// Returns `RagError::Qdrant` on client failures.
pub async fn search_by_vector(
    cfg: &RagConfig,
    client: &QdrantFacade,
    query_vector: Vec<f32>,
    top_k: u64,
    filter: Option<Filter>,
    with_payload: bool,
    exact: bool,
) -> Result<Vec<(f32, serde_json::Value)>, RagError> {
    trace!("retrieve::search_by_vector top_k={top_k} with_payload={with_payload} exact={exact}");
    let res = client
        .search(query_vector, top_k, filter, with_payload, exact)
        .await?;
    Ok(res)
}

/// Embeds the query text and returns normalized RAG context hits.
///
/// # Errors
/// Returns embedding/provider errors or Qdrant failures.
pub async fn rag_context(
    cfg: &RagConfig,
    client: &QdrantFacade,
    query: RagQuery<'_>,
    provider: &dyn EmbeddingsProvider,
) -> Result<Vec<RagHit>, RagError> {
    trace!(
        "retrieve::rag_context top_k={} filter={}",
        query.top_k,
        query.filter.is_some()
    );

    let qv = provider.embed(query.text)?;
    let filter = query.filter.as_ref().map(to_qdrant_filter);

    let hits = search_by_vector(
        cfg,
        client,
        qv,
        query.top_k,
        filter,
        /* with_payload = */ true,
        cfg.exact_search,
    )
    .await?;

    let mut out = Vec::with_capacity(hits.len());
    for (score, payload) in hits {
        let text = payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let source = payload
            .get("source")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        out.push(RagHit {
            score,
            text,
            source,
            payload,
        });
    }

    trace!("retrieve::rag_context hits={}", out.len());
    Ok(out)
}
