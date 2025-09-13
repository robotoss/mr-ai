//! Retrieval-only API: get context chunks from the vector store without chatting.
//!
//! This mirrors the first half of `ask_with_opts` but stops after MMR/expansion,
//! returning selected chunks for downstream consumers (e.g., code-review prompts).

use std::sync::Arc;

use crate::api_types::UsedChunk;
use crate::cfg::ContextorConfig;
use crate::error::ContextorError;
use crate::select;
use ai_llm_service::service_profiles::LlmServiceProfiles;
use rag_store::{
    RagQuery, RagStore,
    embed::ollama::{OllamaConfig, OllamaEmbedder},
};

/// Options to control retrieval behavior. Zero values are replaced by env defaults.
#[derive(Debug, Clone, Default)]
pub struct RetrieveOptions {
    /// Initial candidates from the vector store.
    pub top_k: u64,
    /// Final number of chunks to return after MMR (and expansion if enabled).
    pub context_k: usize,
}

/// Retrieve top context chunks (MMR-selected, optionally neighbor-expanded), no chat.
///
/// This uses the same environment-driven config as `ask_with_opts`, including
/// the embedder model and store connection.
pub async fn retrieve_with_opts(
    query_text: &str,
    opts: RetrieveOptions,
    svc: Arc<LlmServiceProfiles>,
) -> Result<Vec<UsedChunk>, ContextorError> {
    // 1) Config
    let gcfg = ContextorConfig::new(svc);
    let top_k = if opts.top_k == 0 {
        gcfg.initial_top_k
    } else {
        opts.top_k
    };
    let context_k = if opts.context_k == 0 {
        gcfg.context_k
    } else {
        opts.context_k
    };

    // 2) Facades
    let store = RagStore::new(gcfg.make_rag_config())?;
    let emb_cfg = OllamaConfig {
        svc: gcfg.svc.clone(),
        dim: gcfg.make_rag_config().embedding_dim.unwrap_or(1024),
    };
    let embedder = OllamaEmbedder::new(emb_cfg);

    // 3) Retrieve
    let query = RagQuery {
        text: query_text,
        top_k,
        filter: gcfg.initial_filter.clone(),
    };
    let mut hits = store.rag_context(query, &embedder).await?;

    // 4) MMR select
    let selected =
        select::mmr_select(query_text, &embedder, &mut hits, context_k, gcfg.mmr_lambda).await?;

    // 5) Optional neighbor expansion
    let expanded = if gcfg.expand_neighbors {
        select::maybe_expand_neighbors(
            &store,
            &embedder,
            &selected,
            gcfg.neighbor_k,
            gcfg.score_floor,
        )
        .await?
    } else {
        selected
    };

    // 6) Convert for callers (clamped body)
    let items = expanded
        .into_iter()
        .map(|h| {
            let snippet = if h.snippet.is_some() {
                Some(h.snippet.unwrap().clone())
                // Some(rag_store::record::clamp_snippet(
                //     &h.snippet.unwrap(),
                //     800,
                //     100,
                // ))
            } else {
                None
            };
            UsedChunk {
                score: h.score,
                source: h.source,
                fqn: h.fqn,
                kind: h.kind,
                snippet: snippet,
                text: rag_store::record::clamp_snippet(&h.text, 800, 100),
            }
        })
        .collect();

    Ok(items)
}
