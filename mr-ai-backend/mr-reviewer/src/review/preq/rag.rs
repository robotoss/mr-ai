//! RAG adapter: run short queries against the project's vector index,
//! unify result shape, and cap size to keep prompts small.

use std::collections::{HashMap, HashSet};

use super::RagHit;
use crate::errors::MrResult;
use contextor::{RetrieveOptions, retrieve_with_opts};
use serde::{Deserialize, Serialize};

/// Internal representation of a record returned by your vector index.
/// Adjust this to your actual RAG schema (ids, fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexDoc {
    pub id: String,
    pub path: String,
    pub language: Option<String>,
    pub kind: Option<String>,
    pub name: Option<String>,
    pub fqn: Option<String>,
    pub snippet: String,
}

/// Control which channels (queries/paths/symbols) are used for retrieval.
pub struct UseChannels {
    pub use_queries: bool,
    pub use_paths: bool,
    pub use_symbols: bool,
}

impl Default for UseChannels {
    fn default() -> Self {
        Self {
            use_queries: true,
            use_paths: true,
            use_symbols: true,
        }
    }
}

/// High-level fetch that can run channels separately and merge results.
/// Set `UseChannels { use_symbols: true, use_queries: false, use_paths: false }`
/// to run symbols-only retrieval.
pub async fn fetch_context_flexible(
    queries: &[String],
    need_paths_like: &[String],
    need_symbols_like: &[String],
    use_ch: UseChannels,
    final_limit: usize,
) -> MrResult<Vec<RagHit>> {
    let mut syms_hits: Vec<RagHit> = Vec::new();
    let mut path_hits: Vec<RagHit> = Vec::new();
    let mut text_hits: Vec<RagHit> = Vec::new();

    // 1) Symbols-only pass (optional)
    if use_ch.use_symbols {
        syms_hits = fetch_context_symbols_only(need_symbols_like, 3).await?;
    }

    // 2) Paths pass (optional): encode paths into query text and retrieve.
    if use_ch.use_paths && !need_paths_like.is_empty() {
        let q = format!("paths_like: {}", need_paths_like.join(" | "));
        let opts = RetrieveOptions {
            top_k: 6,
            context_k: 6,
            ..Default::default()
        };
        let chunks = retrieve_with_opts(&q, opts)
            .await
            .map_err(|e| crate::errors::Error::Other(format!("contextor failed: {e}")))?;
        path_hits = chunks
            .into_iter()
            .filter_map(|c| {
                c.snippet.map(|s| RagHit {
                    path: c.source.unwrap_or_default(),
                    symbol: c.fqn,
                    language: None,
                    snippet: s,
                    why: format!("paths score={:.3}", c.score),
                })
            })
            .collect();
    }

    // 3) Free-text pass (optional): use `queries` as-is.
    if use_ch.use_queries && !queries.is_empty() {
        let q = queries.join(" || ");
        let opts = RetrieveOptions {
            top_k: 6,
            context_k: 6,
            ..Default::default()
        };
        let chunks = retrieve_with_opts(&q, opts)
            .await
            .map_err(|e| crate::errors::Error::Other(format!("contextor failed: {e}")))?;
        text_hits = chunks
            .into_iter()
            .filter_map(|c| {
                c.snippet.map(|s| RagHit {
                    path: c.source.unwrap_or_default(),
                    symbol: c.fqn,
                    language: None,
                    snippet: s,
                    why: format!("paths score={:.3}", c.score),
                })
            })
            .collect();
    }

    // 4) Weighted merge + dedup + top-N clamp.
    //    You can tune weights to prefer symbol matches.
    let merged = merge_hits_weighted(
        &[
            (&syms_hits, 0.30), // prefer symbols
            (&path_hits, 0.15),
            (&text_hits, 0.10),
        ],
        final_limit,
    );

    Ok(merged)
}

/// Lightweight score container to allow weighted merge from multiple passes.
#[derive(Clone)]
struct ScoredHit {
    hit: RagHit,
    score: f32,
}

/// Run RAG over symbols only, one sub-query per symbol; merge & dedup.
async fn fetch_context_symbols_only(
    symbols: &[String],
    top_k_per_symbol: usize,
) -> MrResult<Vec<RagHit>> {
    // Simple guard
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    // We will deduplicate by (path, snippet) pair.
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut acc: Vec<RagHit> = Vec::new();

    // Use small k per symbol to keep the final RELATED compact.
    let opts = RetrieveOptions {
        top_k: top_k_per_symbol as u64,
        context_k: top_k_per_symbol,
        ..Default::default()
    };

    for sym in symbols {
        // Build a symbol-focused query text. Keep it terse for the embedder.
        let query_text = format!("symbol: {sym}");

        // Call contextor facade → Qdrant.
        let chunks = retrieve_with_opts(&query_text, opts.clone())
            .await
            .map_err(|e| crate::errors::Error::Other(format!("contextor failed: {e}")))?;

        for c in chunks {
            // Skip entries without snippet
            if let Some(snippet) = c.snippet {
                let path = c.source.unwrap_or_default();

                if !seen.insert((path.clone(), snippet.clone())) {
                    continue;
                }

                // Optional defensive filter: ensure the snippet actually contains the symbol token.
                let contains = snippet.to_lowercase().contains(&sym.to_lowercase());
                if !contains && c.score < 0.60 {
                    continue;
                }

                acc.push(RagHit {
                    path,
                    symbol: c.fqn,
                    language: None,
                    snippet, // already owned String
                    why: format!("symbol:{sym} score={:.3}", c.score),
                });
            }
        }
    }

    Ok(acc)
}

/// Merge multiple hit lists with weights, dedup by (path, snippet), and return top-N by score.
fn merge_hits_weighted(
    lists: &[(&[RagHit], f32)], // (hits, weight)
    limit: usize,
) -> Vec<RagHit> {
    let mut by_key: HashMap<(String, String), ScoredHit> = HashMap::new();

    for (hits, w) in lists {
        for h in *hits {
            let key = (h.path.clone(), h.snippet.clone());
            let entry = by_key.entry(key).or_insert_with(|| ScoredHit {
                hit: h.clone(),
                // Baseline score derived from why-field if it contains "score=…"; fallback to 0.5
                score: extract_score(&h.why).unwrap_or(0.5),
            });
            entry.score += *w; // apply weight
        }
    }

    // Rank: higher score first
    let mut all: Vec<ScoredHit> = by_key.into_values().collect();
    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    all.into_iter().take(limit).map(|s| s.hit).collect()
}

/// Try to parse `score=0.713` out of the "why" field.
fn extract_score(why: &str) -> Option<f32> {
    let idx = why.find("score=")?;
    let s = &why[idx + "score=".len()..];
    let end = s
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(s.len());
    s[..end].parse().ok()
}
