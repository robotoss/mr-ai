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
//! Sprint M5 (post-review) improvements:
//! - Chunks are stored in a lightweight projection
//!   ([`EmbeddedChunkLite`]) — only fields the prompt needs. Cuts
//!   per-cache memory ~5–10× compared to cloning full `CodeChunk`.
//! - Embed batches run with `buffer_unordered` so build latency is
//!   `~max(batch_rtt)` instead of `Σ batch_rtt`.
//! - The hot query path is split: [`top_k_with_vector`] is sync and
//!   takes a pre-embedded query so the worker can embed all
//!   per-target queries in one batched call.
//!
//! Failure handling: every step is best-effort. Embed errors yield
//! empty rank-by-cosine results for that target; the review still
//! proceeds with the un-augmented hits.

use std::sync::Arc;

use ai_llm_service::{EmbeddingRequest, EmbeddingTier, LlmGateway};
use domain::RepoId;
use futures::stream::{self, StreamExt};
use rag_base::structs::rag_base_config::RagConfig;
use rag_base::structs::rag_store::SearchHit;
use tracing::warn;

use super::OverlayGraph;

/// Max number of in-flight embed batches when building the cache.
/// Bounded to be polite to the embedding provider's per-second limit
/// — most providers (OpenAI, Bedrock) tolerate ~10 concurrent calls.
const EMBED_BATCH_CONCURRENCY: usize = 8;

/// Cached overlay embeddings. Build once after `build_for_mr`, query
/// many times — once per review target.
pub struct OverlayEmbedCache {
    embeddings: Vec<EmbeddedChunkLite>,
}

/// Projection of [`code_indexer::CodeChunk`] limited to fields the
/// prompt builder actually reads from a [`SearchHit`]. Storing the
/// full chunk would burn ~2KB × 5000 = 10MB per overlay; this trims
/// it to a handful of `String`s + the vector.
#[derive(Debug, Clone)]
struct EmbeddedChunkLite {
    id: String,
    file: String,
    language: String,
    chunk_kind: Option<String>,
    symbol_path: String,
    symbol: String,
    snippet: Option<String>,
    /// Sprint M5: which sibling repo this chunk came from. Surfaces
    /// through `SearchHit.repo_id` so audit / LLM can attribute the
    /// hint back to its source.
    repo_id: Option<RepoId>,
    vector: Vec<f32>,
    norm: f32,
}

impl OverlayEmbedCache {
    /// Embed every chunk in the overlay graph. Returns an empty
    /// cache when the overlay is empty so the downstream
    /// [`Self::top_k_with_vector`] / [`Self::top_k_for_query`]
    /// degrade cleanly.
    ///
    /// Batches embed concurrently (up to [`EMBED_BATCH_CONCURRENCY`])
    /// — large overlays no longer pay `N × batch_rtt` of latency.
    pub async fn build(
        overlay: &OverlayGraph,
        gateway: Arc<LlmGateway>,
        rag_cfg: &RagConfig,
    ) -> Self {
        let chunks: Vec<&code_indexer::CodeChunk> = overlay.new_chunks.values().collect();
        if chunks.is_empty() {
            return Self {
                embeddings: Vec::new(),
            };
        }
        let batch_size = rag_cfg.qdrant.batch_size.max(1);
        let expected_dim = rag_cfg.embedding.dim;
        // Resolve repo attribution once, up-front. The parallel
        // embedding stream then operates on owned data only — no
        // borrow of `overlay` crosses an `.await` point, which
        // sidesteps HRTB issues for callers that wrap us in
        // `tracing::instrument`.
        let prepped: Vec<PreppedChunk> = chunks
            .iter()
            .map(|c| PreppedChunk {
                id: c.id.clone(),
                file: c.file.clone(),
                language: format!("{:?}", c.language).to_lowercase(),
                chunk_kind: c.chunk_kind.map(|k| k.as_str().to_owned()),
                symbol_path: c.symbol_path.clone(),
                symbol: c.symbol.clone(),
                snippet: c.snippet.clone(),
                repo_id: overlay.repo_of_chunk(&c.id),
            })
            .collect();
        drop(chunks);

        let batches: Vec<Vec<PreppedChunk>> =
            prepped.chunks(batch_size).map(|c| c.to_vec()).collect();

        let results: Vec<Vec<EmbeddedChunkLite>> = stream::iter(batches)
            .map(|batch| {
                let gw = gateway.clone();
                async move { embed_one_batch(batch, gw, expected_dim).await }
            })
            .buffer_unordered(EMBED_BATCH_CONCURRENCY)
            .collect()
            .await;

        let embeddings = results.into_iter().flatten().collect();
        Self { embeddings }
    }

    pub fn is_empty(&self) -> bool {
        self.embeddings.is_empty()
    }

    /// Rank cached chunks against a pre-embedded query vector. Sync —
    /// no LLM round-trip on the hot path. Returns up to `k` chunks
    /// with score ≥ `min_score`, sorted descending by cosine.
    /// Empty cache, `k == 0`, or zero-norm query vector → empty
    /// result.
    pub fn top_k_with_vector(
        &self,
        query_vec: &[f32],
        k: usize,
        min_score: f32,
    ) -> Vec<SearchHit> {
        if self.embeddings.is_empty() || k == 0 {
            return Vec::new();
        }
        let q_norm = norm(query_vec);
        if q_norm == 0.0 {
            return Vec::new();
        }
        // Score, filter by threshold, then partial-sort top-k.
        let mut scored: Vec<(f32, &EmbeddedChunkLite)> = self
            .embeddings
            .iter()
            .map(|e| (cosine_pre(query_vec, &e.vector, q_norm, e.norm), e))
            .filter(|(s, _)| *s >= min_score)
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(k);
        scored
            .into_iter()
            .map(|(score, e)| chunk_to_hit(score, e))
            .collect()
    }

    /// Convenience wrapper that embeds `query` then delegates to
    /// [`Self::top_k_with_vector`]. Prefer the sync version when
    /// scoring multiple targets against the same cache — embed all
    /// queries in one batch first.
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
        self.top_k_with_vector(&query_vec, k, min_score)
    }
}

/// Owned, light-weight per-chunk data passed into the parallel embed
/// stream. Decouples [`OverlayEmbedCache::build`] from any borrow of
/// the source [`OverlayGraph`] so each batch future is fully `'static`
/// + `Send`.
#[derive(Debug, Clone)]
struct PreppedChunk {
    id: String,
    file: String,
    language: String,
    chunk_kind: Option<String>,
    symbol_path: String,
    symbol: String,
    snippet: Option<String>,
    repo_id: Option<RepoId>,
}

/// Embed one batch of pre-projected chunks. Returns an empty `Vec` on
/// any embed-side failure — never propagates errors so the overall
/// `build` can keep going on the remaining batches.
async fn embed_one_batch(
    batch: Vec<PreppedChunk>,
    gateway: Arc<LlmGateway>,
    expected_dim: usize,
) -> Vec<EmbeddedChunkLite> {
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
            return Vec::new();
        }
    };
    if resp.vectors.len() != batch.len() {
        warn!(
            target = "overlay.merge",
            got = resp.vectors.len(),
            expected = batch.len(),
            "overlay embed count mismatch; dropping batch"
        );
        return Vec::new();
    }
    let mut out: Vec<EmbeddedChunkLite> = Vec::with_capacity(batch.len());
    for (chunk, vec) in batch.into_iter().zip(resp.vectors.into_iter()) {
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
        out.push(EmbeddedChunkLite {
            id: chunk.id,
            file: chunk.file,
            language: chunk.language,
            chunk_kind: chunk.chunk_kind,
            symbol_path: chunk.symbol_path,
            symbol: chunk.symbol,
            snippet: chunk.snippet,
            repo_id: chunk.repo_id,
            vector: vec,
            norm: n,
        });
    }
    out
}

fn chunk_to_hit(score: f32, c: &EmbeddedChunkLite) -> SearchHit {
    SearchHit {
        score,
        id: c.id.clone(),
        file: c.file.clone(),
        language: c.language.clone(),
        kind: c.chunk_kind.clone().unwrap_or_default(),
        symbol_path: c.symbol_path.clone(),
        symbol: c.symbol.clone(),
        signature: None,
        snippet: c.snippet.clone(),
        chunk_kind: c.chunk_kind.clone(),
        parent_symbol_id: None,
        // Sprint M5: surface sibling repo attribution so audit /
        // downstream LLM can see which repo contributed this hint.
        // Serialised as the UUID simple-form to match the rest of
        // the multi-tenant payload model.
        repo_id: c.repo_id.map(|r| r.as_uuid().simple().to_string()),
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

    fn lite(id: &str, repo_id: Option<RepoId>, vector: Vec<f32>) -> EmbeddedChunkLite {
        let n = norm(&vector);
        EmbeddedChunkLite {
            id: id.into(),
            file: format!("{id}.rs"),
            language: "rust".into(),
            chunk_kind: Some("Symbol".into()),
            symbol_path: format!("{id}::sym"),
            symbol: id.into(),
            snippet: Some(format!("snippet for {id}")),
            repo_id,
            vector,
            norm: n,
        }
    }

    #[test]
    fn empty_cache_returns_empty_for_any_query() {
        let cache = OverlayEmbedCache {
            embeddings: Vec::new(),
        };
        assert!(cache.is_empty());
        let hits = cache.top_k_with_vector(&[1.0, 0.0, 0.0], 5, 0.0);
        assert!(hits.is_empty());
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

    #[test]
    fn top_k_with_vector_ranks_by_cosine_and_truncates() {
        // Three chunks with deterministic vectors so the cosine order
        // is unambiguous: needle aligned to (1,0,0), good (0.9,0.1,0),
        // mid (0.5,0.5,0), bad (0,1,0).
        let cache = OverlayEmbedCache {
            embeddings: vec![
                lite("good", None, vec![0.9, 0.1, 0.0]),
                lite("mid", None, vec![0.5, 0.5, 0.0]),
                lite("bad", None, vec![0.0, 1.0, 0.0]),
            ],
        };
        let hits = cache.top_k_with_vector(&[1.0, 0.0, 0.0], 2, 0.0);
        assert_eq!(hits.len(), 2);
        // Sorted desc by cosine: good > mid > bad. Top-2 keeps {good, mid}.
        assert_eq!(hits[0].id, "good");
        assert_eq!(hits[1].id, "mid");
        assert!(hits[0].score >= hits[1].score);
    }

    #[test]
    fn top_k_with_vector_filters_by_min_score() {
        let cache = OverlayEmbedCache {
            embeddings: vec![
                lite("good", None, vec![1.0, 0.0, 0.0]),
                lite("orthogonal", None, vec![0.0, 1.0, 0.0]),
            ],
        };
        // min_score = 0.5 excludes the orthogonal chunk (cosine 0).
        let hits = cache.top_k_with_vector(&[1.0, 0.0, 0.0], 10, 0.5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "good");
    }

    #[test]
    fn top_k_with_vector_zero_norm_query_returns_empty() {
        let cache = OverlayEmbedCache {
            embeddings: vec![lite("any", None, vec![1.0, 0.0, 0.0])],
        };
        let hits = cache.top_k_with_vector(&[0.0, 0.0, 0.0], 10, 0.0);
        assert!(hits.is_empty(), "zero-norm query must NOT match anything");
    }

    #[test]
    fn top_k_with_vector_propagates_repo_id_attribution() {
        // Sprint M5 #13: overlay-built chunks should carry the
        // originating repo id through to the SearchHit so audit /
        // LLM can attribute hints back to a sibling repo.
        let repo = RepoId::new();
        let cache = OverlayEmbedCache {
            embeddings: vec![lite("attributed", Some(repo), vec![1.0, 0.0, 0.0])],
        };
        let hits = cache.top_k_with_vector(&[1.0, 0.0, 0.0], 1, 0.0);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].repo_id.as_deref(),
            Some(repo.as_uuid().simple().to_string().as_str())
        );
    }
}
