//! Merge overlay chunks into RAG hits via cosine similarity.
//!
//! Sprint M2 of cross-repo MR review. The overlay (S7) collects code
//! from sibling repos at MR time; this helper turns those raw chunks
//! into ranked [`SearchHit`]s so the RAG layer can append them to the
//! prompt's per-target context.
//!
//! Why a separate helper:
//! - [`build_two_phase_review`] is called from the worker, not from
//!   `/retrieve`, so we need a path that doesn't depend on the HTTP
//!   request shape.
//! - Embeddings of the overlay are expensive (one batched LLM call
//!   per ~64 chunks); they're computed **once** at build time and
//!   reused across every review target's query.
//!
//! Failure handling: every step is best-effort. Embed errors yield
//! empty rank-by-cosine results for that target; the review still
//! proceeds with the un-augmented hits.

use std::sync::Arc;

use ai_llm_service::{EmbeddingRequest, EmbeddingTier, LlmGateway};
use rag_base::structs::rag_base_config::RagConfig;
use rag_base::structs::rag_store::SearchHit;
use tracing::warn;

use super::OverlayGraph;

/// Cached overlay embeddings. Build once after `build_for_mr`, query
/// many times — once per review target.
pub struct OverlayEmbedCache {
    embeddings: Vec<EmbeddedChunk>,
}

struct EmbeddedChunk {
    chunk: code_indexer::CodeChunk,
    vector: Vec<f32>,
    norm: f32,
}

impl OverlayEmbedCache {
    /// Embed every chunk in the overlay graph. Returns an empty
    /// cache when the overlay is empty so the downstream `top_k_for_query`
    /// degrades cleanly.
    pub async fn build(
        overlay: &OverlayGraph,
        gateway: Arc<LlmGateway>,
        rag_cfg: &RagConfig,
    ) -> Self {
        let chunks: Vec<&code_indexer::CodeChunk> = overlay.new_chunks.values().collect();
        if chunks.is_empty() {
            return Self { embeddings: Vec::new() };
        }
        let batch_size = rag_cfg.qdrant.batch_size.max(1);
        let expected_dim = rag_cfg.embedding.dim;
        let mut out: Vec<EmbeddedChunk> = Vec::with_capacity(chunks.len());
        for batch in chunks.chunks(batch_size) {
            let texts: Vec<String> = batch
                .iter()
                .map(|c| {
                    c.snippet.clone().unwrap_or_else(|| {
                        warn!(
                            target = "overlay.merge",
                            chunk = %c.symbol_path,
                            "overlay chunk has no snippet; embedding symbol_path as last resort"
                        );
                        c.symbol_path.clone()
                    })
                })
                .collect();
            let resp = match gateway
                .embed_batch(EmbeddingTier::Default, EmbeddingRequest::new(texts))
                .await
            {
                Ok(r) => r,
                Err(err) => {
                    warn!(
                        target = "overlay.merge",
                        error = %err,
                        batch_size = batch.len(),
                        "overlay embed_batch failed; dropping batch"
                    );
                    continue;
                }
            };
            if resp.vectors.len() != batch.len() {
                warn!(
                    target = "overlay.merge",
                    got = resp.vectors.len(),
                    expected = batch.len(),
                    "overlay embed count mismatch; dropping batch"
                );
                continue;
            }
            for (chunk, vec) in batch.iter().zip(resp.vectors.into_iter()) {
                if vec.len() != expected_dim {
                    warn!(
                        target = "overlay.merge",
                        chunk = %chunk.symbol_path,
                        got = vec.len(),
                        expected = expected_dim,
                        "overlay embed dim mismatch; skipping chunk"
                    );
                    continue;
                }
                let n = norm(&vec);
                out.push(EmbeddedChunk {
                    chunk: (*chunk).clone(),
                    vector: vec,
                    norm: n,
                });
            }
        }
        Self { embeddings: out }
    }

    pub fn is_empty(&self) -> bool {
        self.embeddings.is_empty()
    }

    /// Embed `query` and return up to `k` chunks ranked by cosine
    /// similarity, score ≥ `min_score`. Empty cache → empty result.
    /// Best-effort: embed failure → empty result, never errors.
    pub async fn top_k_for_query(
        &self,
        gateway: Arc<LlmGateway>,
        query: &str,
        k: usize,
        min_score: f32,
    ) -> Vec<SearchHit> {
        if self.embeddings.is_empty() || k == 0 {
            return Vec::new();
        }
        let resp = match gateway
            .embed_batch(
                EmbeddingTier::Default,
                EmbeddingRequest::new(vec![query.to_owned()]),
            )
            .await
        {
            Ok(r) => r,
            Err(err) => {
                warn!(
                    target = "overlay.merge",
                    error = %err,
                    "query embed_batch failed; returning empty overlay hits"
                );
                return Vec::new();
            }
        };
        let Some(query_vec) = resp.vectors.into_iter().next() else {
            return Vec::new();
        };
        let q_norm = norm(&query_vec);
        if q_norm == 0.0 {
            return Vec::new();
        }
        // Score every chunk; sort once and trim.
        let mut scored: Vec<(f32, &EmbeddedChunk)> = self
            .embeddings
            .iter()
            .map(|e| (cosine_pre(&query_vec, &e.vector, q_norm, e.norm), e))
            .filter(|(s, _)| *s >= min_score)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
            .into_iter()
            .map(|(score, e)| chunk_to_hit(score, &e.chunk))
            .collect()
    }
}

fn chunk_to_hit(score: f32, chunk: &code_indexer::CodeChunk) -> SearchHit {
    SearchHit {
        score,
        id: chunk.id.clone(),
        file: chunk.file.clone(),
        language: format!("{:?}", chunk.language).to_lowercase(),
        kind: chunk
            .chunk_kind
            .map(|k| k.as_str().to_owned())
            .unwrap_or_default(),
        symbol_path: chunk.symbol_path.clone(),
        symbol: chunk.symbol.clone(),
        signature: None,
        snippet: chunk.snippet.clone(),
        chunk_kind: chunk.chunk_kind.map(|k| k.as_str().to_owned()),
        parent_symbol_id: None,
        repo_id: None,
    }
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn cosine_pre(a: &[f32], b: &[f32], a_norm: f32, b_norm: f32) -> f32 {
    if a_norm == 0.0 || b_norm == 0.0 {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    dot / (a_norm * b_norm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache_returns_empty_for_any_query() {
        let cache = OverlayEmbedCache {
            embeddings: Vec::new(),
        };
        assert!(cache.is_empty());
    }

    #[test]
    fn cosine_helpers_match_known_pairs() {
        let a = [1.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        let s = cosine_pre(&a, &b, norm(&a), norm(&b));
        assert!((s - 1.0).abs() < 1e-6);

        let c = [0.0, 1.0, 0.0];
        let s2 = cosine_pre(&a, &c, norm(&a), norm(&c));
        assert!(s2.abs() < 1e-6);

        // zero-norm vector → 0.0 (safe)
        let z = [0.0, 0.0, 0.0];
        let s3 = cosine_pre(&z, &a, norm(&z), norm(&a));
        assert_eq!(s3, 0.0);
    }
}
