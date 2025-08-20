//! Candidate selection (MMR) and neighbor expansion using rag-store.

use crate::error::ContextorError;
use rag_store::{EmbeddingsProvider, RagFilter, RagHit, RagStore};
use serde_json::json;

/// Select top-N diverse chunks using Maximal Marginal Relevance (MMR).
///
/// The function embeds the question and candidates (or reuses stored vectors),
/// then balances relevance to the question with diversity among selected items.
/// Setting `lambda` closer to 1.0 prefers relevance; closer to 0.0 prefers
/// diversity.
///
/// # Errors
/// Propagates embedding errors from the provider.
///
/// # Example
/// ```no_run
/// # use rag_store::{RagHit, embed::ollama::{OllamaConfig, OllamaEmbedder}};
/// # use contextor::select::mmr_select;
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let emb = OllamaEmbedder::new(OllamaConfig{
///     host: "http://127.0.0.1:11434".into(),
///     model: "dengcao/Qwen3-Embedding-0.6B:Q8_0".into(),
///     ..Default::default()
/// })?;
/// let mut hits: Vec<RagHit> = vec![]; // fill from rag-store
/// let picked = mmr_select("my question", &emb, &mut hits, 6, 0.7).await?;
/// assert!(picked.len() <= 6);
/// # Ok(()) }
/// ```
pub async fn mmr_select(
    question: &str,
    provider: &dyn EmbeddingsProvider,
    hits: &mut [RagHit],
    n: usize,
    lambda: f32,
) -> Result<Vec<RagHit>, ContextorError> {
    let qvec = provider.embed(question).await?;

    // Precompute/collect candidate embeddings.
    let mut cand_vecs: Vec<Vec<f32>> = Vec::with_capacity(hits.len());
    for h in hits.iter() {
        if let Some(vec) = h
            .raw_payload
            .get("embedding")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_f64())
                    .map(|f| f as f32)
                    .collect()
            })
        {
            cand_vecs.push(vec);
        } else {
            cand_vecs.push(provider.embed(&h.text).await?);
        }
    }

    // Sort by relevance score (desc) and pre-limit to ~3N.
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    let prelimit = (n * 3).min(hits.len());
    let mut remaining: Vec<usize> = (0..prelimit).collect();
    let mut selected: Vec<usize> = Vec::new();

    while selected.len() < n && !remaining.is_empty() {
        let best = remaining
            .iter()
            .copied()
            .max_by(|&i, &j| {
                let a = mmr_gain(&qvec, i, &selected, &cand_vecs, hits, lambda);
                let b = mmr_gain(&qvec, j, &selected, &cand_vecs, hits, lambda);
                a.total_cmp(&b)
            })
            .unwrap();
        selected.push(best);
        remaining.retain(|&x| x != best);
    }

    // Keep order by original score among selected.
    selected.sort_by_key(|&i| std::cmp::Reverse((hits[i].score.to_bits(), i)));
    Ok(selected.into_iter().map(|i| hits[i].clone()).collect())
}

fn mmr_gain(
    q: &[f32],
    idx: usize,
    selected: &[usize],
    cand_vecs: &[Vec<f32>],
    hits: &[RagHit],
    lambda: f32,
) -> f32 {
    let v = &cand_vecs[idx];
    let rel = cosine(q, v);

    let div = if selected.is_empty() {
        0.0
    } else {
        // minimize similarity to already picked items
        let mut worst_sim = 1.0f32;
        for &s in selected {
            let sim = cosine(v, &cand_vecs[s]);
            worst_sim = worst_sim.min(sim);
        }
        worst_sim
    };

    lambda * rel + (1.0 - lambda) * (1.0 - div) + hits[idx].score * 1e-3
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    let len = a.len().min(b.len());
    for i in 0..len {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Expand the selected context with neighbors in the same `source` or `fqn`.
///
/// For each selected hit above `score_floor`, we run a small local search
/// near the hit vector (reusing its embedding if present), restricted by a
/// filter `source == <same>` or `fqn == <same>`.
///
/// The result is deduplicated (by `{source,fqn,text}`), re-sorted by score,
/// and trimmed to `~2 * selected.len()`.
///
/// # Errors
/// Propagates `rag-store` errors from `search_by_vector` and embedding.
///
/// # Example
/// ```no_run
/// # use rag_store::{RagStore, RagHit, embed::ollama::{OllamaConfig, OllamaEmbedder}};
/// # use contextor::select::maybe_expand_neighbors;
/// # async fn run(store: RagStore) -> Result<(), Box<dyn std::error::Error>> {
/// let emb = OllamaEmbedder::new(OllamaConfig{
///     host: "http://127.0.0.1:11434".into(),
///     model: "dengcao/Qwen3-Embedding-0.6B:Q8_0".into(),
///     ..Default::default()
/// })?;
/// let selected: Vec<RagHit> = vec![]; // filled earlier
/// let expanded = maybe_expand_neighbors(&store, &emb, &selected, 6, 0.0).await?;
/// # Ok(()) }
/// ```
pub async fn maybe_expand_neighbors(
    store: &RagStore,
    provider: &dyn EmbeddingsProvider,
    selected: &[RagHit],
    neighbor_k: u64,
    score_floor: f32,
) -> Result<Vec<RagHit>, ContextorError> {
    let mut out = Vec::new();

    for h in selected {
        out.push(h.clone());
        if h.score < score_floor {
            continue;
        }

        // Reuse embedding if present; otherwise embed the text.
        let vec = if let Some(v) = h
            .raw_payload
            .get("embedding")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_f64())
                    .map(|f| f as f32)
                    .collect()
            }) {
            v
        } else {
            provider.embed(&h.text).await?
        };

        // Prefer restricting by `source`, fallback to `fqn`.
        let filter = if let Some(src) = &h.source {
            Some(RagFilter {
                equals: vec![("source".into(), json!(src))],
            })
        } else if let Some(fqn) = &h.fqn {
            Some(RagFilter {
                equals: vec![("fqn".into(), json!(fqn))],
            })
        } else {
            None
        };

        // Local vector search around the hit.
        let neighs = store
            .search_by_vector(vec, neighbor_k, filter, /*with_payload*/ true)
            .await?;

        for (score, payload) in neighs {
            let mut nh = payload_to_hit(payload);
            nh.score = score;

            // Dedup by tuple (source, fqn, text).
            if out
                .iter()
                .any(|x| x.source == nh.source && x.fqn == nh.fqn && x.text == nh.text)
            {
                continue;
            }
            out.push(nh);
        }
    }

    // Sort by score and trim.
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate((selected.len() * 2).max(selected.len()));
    Ok(out)
}

fn payload_to_hit(payload: serde_json::Value) -> RagHit {
    use serde_json::Value as J;

    let (mut text, mut source, mut language, mut kind, mut fqn, mut snippet) =
        (String::new(), None, None, None, None, None);
    let mut tags = Vec::<String>::new();
    let mut metrics = None;

    if let J::Object(m) = &payload {
        text = m
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        source = m
            .get("source")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        language = m
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        kind = m
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        fqn = m.get("fqn").and_then(|v| v.as_str()).map(|s| s.to_string());
        snippet = m
            .get("snippet")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                m.get("context_snippet")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
        if let Some(a) = m.get("tags").and_then(|v| v.as_array()) {
            tags = a
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
        }
        metrics = m.get("metrics").cloned();
    }

    RagHit {
        score: 0.0,
        text,
        snippet,
        source,
        language,
        kind,
        fqn,
        tags,
        neighbors: Vec::new(),
        metrics,
        raw_payload: payload,
    }
}
