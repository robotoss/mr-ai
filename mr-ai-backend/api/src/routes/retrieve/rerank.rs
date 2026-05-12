//! `/retrieve?rerank=true` plumbing — cache lookup + LLM rerank fallback.
//!
//! The Smart-tier rerank costs $0.001-0.005 per call and ~500ms of
//! latency, so a Postgres-backed cache short-circuits repeated calls
//! within `RERANK_CACHE_TTL_HOURS`. Cache key is sha256 of
//! (query + project_id + repo_id + top_k + sorted chunk_ids).

use std::sync::Arc;
use std::time::Duration;

use ai_llm_service::LlmGateway;
use domain::RetrievalConfig;
use git_context_engine::review::retrieval::plan::{
    RetrievalPlan, RetrievalSeed, ScoredHit, SeedSource,
};
use git_context_engine::review::retrieval::llm_rerank::llm_rerank;
use persistence::repos::rerank_cache;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{debug, warn};

use crate::routes::retrieve::response::{RetrievedHit, Via};

/// Compute the deterministic cache key. Inputs are canonicalised
/// (sorted chunk_ids; explicit repo placeholder) so two requests with
/// the same logical input always hash to the same key.
pub fn cache_key(
    query: &str,
    project_id: &str,
    repo_id: Option<&str>,
    rerank_top_k: usize,
    hits: &[RetrievedHit],
) -> String {
    let mut ids: Vec<&str> = hits.iter().map(|h| h.chunk_id.as_str()).collect();
    ids.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(b"v1\x00");
    hasher.update(query.as_bytes());
    hasher.update(b"\x00");
    hasher.update(project_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(repo_id.unwrap_or("-").as_bytes());
    hasher.update(b"\x00");
    hasher.update(rerank_top_k.to_le_bytes());
    hasher.update(b"\x00");
    for id in &ids {
        hasher.update(id.as_bytes());
        hasher.update(b",");
    }
    let digest = hasher.finalize();
    hex_encode(&digest)
}

/// Build a `RetrievalPlan` from the merged hit set. Vector→Vector,
/// Graph→Lexical (closest existing SeedSource variant for graph hops),
/// Overlay→Overlay. Scores carry through unchanged so the LLM sees
/// the actual ranking input.
pub fn plan_from_hits(query: &str, hits: &[RetrievedHit]) -> RetrievalPlan {
    let mut plan = RetrievalPlan::new(RetrievalConfig::from_env());
    plan.query = Some(query.to_owned());
    for h in hits {
        plan.add_seed(RetrievalSeed {
            chunk_id: h.chunk_id.clone(),
            file: h.file.clone(),
            symbol_path: h.symbol_path.clone(),
            score: h.score,
            source: map_via(&h.via),
        });
    }
    plan
}

fn map_via(via: &Via) -> SeedSource {
    match via {
        Via::Vector => SeedSource::Vector,
        Via::Graph => SeedSource::Lexical,
        Via::Overlay => SeedSource::Overlay,
        Via::Lexical => SeedSource::Lexical,
    }
}

/// Reorder `hits` by the `ScoredHit` ranking. Hits whose `chunk_id`
/// isn't present in `ranked` keep their position relative to each
/// other but land *after* every ranked hit.
pub fn reorder_by_ranked(hits: &mut Vec<RetrievedHit>, ranked: &[ScoredHit]) {
    let order: std::collections::HashMap<&str, usize> = ranked
        .iter()
        .enumerate()
        .map(|(i, h)| (h.chunk_id.as_str(), i))
        .collect();
    let scores: std::collections::HashMap<&str, f32> = ranked
        .iter()
        .map(|h| (h.chunk_id.as_str(), h.score))
        .collect();
    hits.sort_by(|a, b| {
        let pos_a = order.get(a.chunk_id.as_str()).copied().unwrap_or(usize::MAX);
        let pos_b = order.get(b.chunk_id.as_str()).copied().unwrap_or(usize::MAX);
        pos_a.cmp(&pos_b)
    });
    // Project the LLM's rescaled score onto the hit so the caller can
    // see what the rerank actually produced (and so the diagnostic
    // bundle in the worker carries the rerank float).
    for hit in hits.iter_mut() {
        if let Some(score) = scores.get(hit.chunk_id.as_str()) {
            hit.score = *score;
        }
    }
}

/// Wrap an existing rerank call in cache lookup / store. Returns the
/// reordered hits + a flag telling the caller whether the value came
/// from the cache (`true`) or required an LLM call (`false`).
#[allow(clippy::too_many_arguments)]
pub async fn rerank_with_cache(
    pool: Option<&PgPool>,
    gateway: Arc<LlmGateway>,
    timeout: Duration,
    ttl_hours: i64,
    query: &str,
    project_id: &str,
    repo_id: Option<&str>,
    rerank_top_k: usize,
    hits: &mut Vec<RetrievedHit>,
) -> bool {
    if hits.is_empty() {
        return false;
    }
    let key = cache_key(query, project_id, repo_id, rerank_top_k, hits);

    // Cache hit short-circuits the LLM call entirely.
    if let Some(pool) = pool {
        match rerank_cache::lookup(pool, &key).await {
            Ok(Some(entry)) => {
                if let Ok(ranked) = serde_json::from_value::<Vec<ScoredHit>>(entry.hits_json) {
                    reorder_by_ranked(hits, &ranked);
                    debug!(
                        target = "retrieve.rerank",
                        cache_key = %key,
                        "rerank cache hit"
                    );
                    return true;
                } else {
                    warn!(
                        target = "retrieve.rerank",
                        cache_key = %key,
                        "rerank cache row failed to deserialize; recomputing"
                    );
                }
            }
            Ok(None) => {}
            Err(err) => {
                warn!(
                    target = "retrieve.rerank",
                    error = %err,
                    "rerank cache lookup failed; falling through to LLM"
                );
            }
        }
    }

    // Miss → invoke the Smart-tier rerank and persist the result.
    let plan = plan_from_hits(query, hits);
    let ranked = llm_rerank(gateway, &plan, timeout).await;
    reorder_by_ranked(hits, &ranked);

    if let Some(pool) = pool {
        // Sprint C2: rerank_cache.project_id is NOT NULL; parse the
        // string-form project_id (simple or hyphenated) back into a
        // typed `ProjectId`. The string was minted from a valid UUID
        // upstream — parse failure is treated as a soft cache miss
        // (we log + continue without persisting).
        match uuid::Uuid::parse_str(project_id).map(domain::ProjectId::from_uuid) {
            Ok(pid) => match serde_json::to_value(&ranked) {
                Ok(hits_json) => {
                    if let Err(err) =
                        rerank_cache::upsert(pool, &key, pid, &hits_json, ttl_hours).await
                    {
                        warn!(
                            target = "retrieve.rerank",
                            error = %err,
                            "rerank cache upsert failed; result still returned"
                        );
                    }
                }
                Err(err) => warn!(
                    target = "retrieve.rerank",
                    error = %err,
                    "rerank cache serialize failed; result still returned"
                ),
            },
            Err(err) => warn!(
                target = "retrieve.rerank",
                error = %err,
                project_id = %project_id,
                "rerank cache: project_id string not a valid UUID; result still returned"
            ),
        }
    }
    false
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(chunk_id: &str, score: f32, via: Via) -> RetrievedHit {
        RetrievedHit {
            chunk_id: chunk_id.into(),
            project_id: "p".into(),
            repo_id: None,
            file: format!("{chunk_id}.rs"),
            symbol_path: format!("{chunk_id}::sym"),
            chunk_kind: None,
            score,
            via,
            hops: 0,
            snippet: None,
        }
    }

    #[test]
    fn cache_key_is_deterministic_across_hit_order() {
        let a = vec![
            hit("ck1", 0.9, Via::Vector),
            hit("ck2", 0.8, Via::Graph),
        ];
        let b = vec![
            hit("ck2", 0.8, Via::Graph),
            hit("ck1", 0.9, Via::Vector),
        ];
        let k_a = cache_key("query", "proj", Some("repo"), 5, &a);
        let k_b = cache_key("query", "proj", Some("repo"), 5, &b);
        assert_eq!(k_a, k_b);
    }

    #[test]
    fn cache_key_differs_for_distinct_inputs() {
        let h = vec![hit("ck1", 0.9, Via::Vector)];
        let k1 = cache_key("query", "proj", Some("repo"), 5, &h);
        let k2 = cache_key("query2", "proj", Some("repo"), 5, &h);
        let k3 = cache_key("query", "proj", None, 5, &h);
        let k4 = cache_key("query", "proj", Some("repo"), 10, &h);
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        assert_ne!(k1, k4);
    }

    #[test]
    fn reorder_by_ranked_promotes_ranked_hits_and_preserves_unranked_tail() {
        let mut hits = vec![
            hit("ck1", 0.9, Via::Vector),
            hit("ck2", 0.8, Via::Vector),
            hit("ck3", 0.7, Via::Graph),
        ];
        // LLM bumped ck3 to first, ck1 second.
        let ranked = vec![
            ScoredHit {
                chunk_id: "ck3".into(),
                file: "ck3.rs".into(),
                symbol_path: "ck3::sym".into(),
                score: 0.99,
                via: SeedSource::Lexical,
                hops: 0,
            },
            ScoredHit {
                chunk_id: "ck1".into(),
                file: "ck1.rs".into(),
                symbol_path: "ck1::sym".into(),
                score: 0.85,
                via: SeedSource::Vector,
                hops: 0,
            },
        ];
        reorder_by_ranked(&mut hits, &ranked);
        assert_eq!(hits[0].chunk_id, "ck3");
        assert_eq!(hits[1].chunk_id, "ck1");
        // ck2 wasn't in the ranking — lands at the tail.
        assert_eq!(hits[2].chunk_id, "ck2");
        // Scores from the LLM rerank carry over.
        assert!((hits[0].score - 0.99).abs() < 1e-6);
        assert!((hits[1].score - 0.85).abs() < 1e-6);
    }
}
