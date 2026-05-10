//! LLM-driven rerank stage.
//!
//! Builds a compact JSON-shaped prompt asking the Smart-tier LLM to score
//! every seed in `[0.0, 1.0]`. Failures (timeout, malformed JSON, missing
//! IDs) fall through to `heuristic_rerank` so the pipeline still produces
//! a result.

use std::sync::Arc;
use std::time::Duration;

use ai_llm_service::{LlmGateway, ModelTier, UnifiedRequest};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};

use crate::retrieval::plan::{heuristic_rerank, RetrievalPlan, ScoredHit};

#[derive(Debug, Error)]
pub enum LlmRerankError {
    #[error("gateway call failed: {0}")]
    Gateway(String),
    #[error("response timed out after {ms}ms")]
    Timeout { ms: u64 },
    #[error("could not parse rerank response: {0}")]
    Parse(String),
}

/// JSON shape we ask the model to emit. One entry per seed, score in [0, 1].
#[derive(Debug, Serialize, Deserialize)]
struct RerankItem {
    chunk_id: String,
    score: f32,
}

#[derive(Debug, Serialize, Deserialize)]
struct RerankEnvelope {
    #[serde(default)]
    items: Vec<RerankItem>,
}

/// Build the prompt for the LLM. Public so handlers can preview / debug it
/// (review-engine reuses this format when streaming the rerank step into
/// the `mr_reviews.bundle` snapshot).
pub fn build_rerank_prompt(plan: &RetrievalPlan) -> String {
    let mut out = String::new();
    out.push_str(
        "You are reranking code retrieval candidates for a code review.\n\
         Return STRICTLY a JSON object of the form:\n\
         {\"items\":[{\"chunk_id\":\"<id>\",\"score\":<float in [0,1]>},...]}\n\
         Only include the chunk_ids supplied below; do not invent new ones.\n\n",
    );
    if let Some(query) = plan.query.as_deref() {
        out.push_str("QUERY:\n");
        out.push_str(query);
        out.push_str("\n\n");
    }
    out.push_str("SEEDS:\n");
    for seed in &plan.seeds {
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "- chunk_id: {}; file: {}; symbol: {}; source: {:?}; current_score: {:.3}\n",
                seed.chunk_id, seed.file, seed.symbol_path, seed.source, seed.score
            ),
        );
    }
    out
}

/// Parse the LLM response. Tolerant of common formatting fluff:
/// - Markdown code fences (```json ... ```).
/// - Leading prose before the JSON object.
/// - Extra fields beyond `items`.
pub fn parse_rerank_response(raw: &str) -> Result<Vec<RerankItem>, LlmRerankError> {
    let stripped = strip_code_fence(raw.trim());
    let json_start = stripped
        .find('{')
        .ok_or_else(|| LlmRerankError::Parse("no JSON object found".into()))?;
    let json_end = stripped
        .rfind('}')
        .ok_or_else(|| LlmRerankError::Parse("missing closing brace".into()))?;
    if json_end < json_start {
        return Err(LlmRerankError::Parse("malformed JSON object".into()));
    }
    let json_slice = &stripped[json_start..=json_end];
    let envelope: RerankEnvelope = serde_json::from_str(json_slice)
        .map_err(|err| LlmRerankError::Parse(err.to_string()))?;
    Ok(envelope.items)
}

fn strip_code_fence(input: &str) -> &str {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.trim_end_matches("```").trim();
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.trim_end_matches("```").trim();
    }
    trimmed
}

fn fold_into_hits(plan: &RetrievalPlan, items: &[RerankItem]) -> Vec<ScoredHit> {
    let mut by_id: std::collections::HashMap<&str, f32> = std::collections::HashMap::new();
    for item in items {
        by_id.insert(item.chunk_id.as_str(), item.score.clamp(0.0, 1.0));
    }
    let mut hits: Vec<ScoredHit> = plan
        .seeds
        .iter()
        .map(|seed| ScoredHit {
            chunk_id: seed.chunk_id.clone(),
            file: seed.file.clone(),
            symbol_path: seed.symbol_path.clone(),
            score: by_id.get(seed.chunk_id.as_str()).copied().unwrap_or(seed.score),
            via: seed.source,
            hops: 0,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.file.cmp(&b.file))
    });
    hits
}

/// Run the LLM rerank. On any failure, fall through to `heuristic_rerank`.
/// The caller decides whether to surface the error for diagnostics; the
/// returned hit list is always non-empty when the plan had seeds.
pub async fn llm_rerank(
    gateway: Arc<LlmGateway>,
    plan: &RetrievalPlan,
    timeout: Duration,
) -> Vec<ScoredHit> {
    if plan.seeds.is_empty() {
        return Vec::new();
    }
    let prompt = build_rerank_prompt(plan);
    let request = UnifiedRequest::user_only(prompt);
    let started = std::time::Instant::now();
    let resp = match tokio::time::timeout(timeout, gateway.complete(ModelTier::Smart, request)).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(err)) => {
            warn!(target = "retrieval.llm_rerank", error = %err, "gateway error; falling back to heuristic");
            return heuristic_rerank(plan);
        }
        Err(_) => {
            warn!(
                target = "retrieval.llm_rerank",
                timeout_ms = timeout.as_millis() as u64,
                "rerank timed out; falling back to heuristic"
            );
            return heuristic_rerank(plan);
        }
    };
    debug!(
        target = "retrieval.llm_rerank",
        elapsed_ms = started.elapsed().as_millis() as u64,
        "rerank complete"
    );
    match parse_rerank_response(&resp.content) {
        Ok(items) if !items.is_empty() => fold_into_hits(plan, &items),
        Ok(_) => {
            warn!(target = "retrieval.llm_rerank", "rerank response had no items; falling back to heuristic");
            heuristic_rerank(plan)
        }
        Err(err) => {
            warn!(target = "retrieval.llm_rerank", error = %err, "rerank parse error; falling back to heuristic");
            heuristic_rerank(plan)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::plan::{RetrievalSeed, SeedSource};
    use domain::RetrievalConfig;

    fn seed(id: &str, score: f32) -> RetrievalSeed {
        RetrievalSeed {
            chunk_id: id.into(),
            file: format!("{id}.dart"),
            symbol_path: format!("{id}::doit"),
            score,
            source: SeedSource::Vector,
        }
    }

    #[test]
    fn parses_clean_json_envelope() {
        let raw = r#"{"items":[{"chunk_id":"a","score":0.9},{"chunk_id":"b","score":0.4}]}"#;
        let items = parse_rerank_response(raw).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].chunk_id, "a");
        assert!((items[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn parses_through_code_fence_and_prose() {
        let raw = r#"Sure, here are the scores:
```json
{"items":[{"chunk_id":"x","score":0.7}]}
```"#;
        let items = parse_rerank_response(raw).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].chunk_id, "x");
    }

    #[test]
    fn parse_error_when_no_json() {
        let raw = "I have no idea what to score.";
        let err = parse_rerank_response(raw).unwrap_err();
        assert!(matches!(err, LlmRerankError::Parse(_)));
    }

    #[test]
    fn parse_error_on_malformed_json() {
        let raw = r#"{"items":[{"chunk_id":"a","score":0.9},"#;
        let err = parse_rerank_response(raw).unwrap_err();
        assert!(matches!(err, LlmRerankError::Parse(_)));
    }

    #[test]
    fn fold_clamps_scores_and_preserves_unscored_seeds() {
        let mut plan = RetrievalPlan::new(RetrievalConfig::default());
        plan.add_seed(seed("a", 0.3));
        plan.add_seed(seed("b", 0.5));
        let items = vec![
            RerankItem {
                chunk_id: "a".into(),
                score: 1.7, // out of range; clamps to 1.0
            },
            // "b" is left unscored — falls back to seed score.
        ];
        let hits = fold_into_hits(&plan, &items);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk_id, "a"); // top after clamp
        assert!((hits[0].score - 1.0).abs() < 1e-6);
        let b = hits.iter().find(|h| h.chunk_id == "b").unwrap();
        assert!((b.score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn build_prompt_includes_query_and_seeds() {
        let mut plan = RetrievalPlan::new(RetrievalConfig::default());
        plan.query = Some("review the diff in router.dart".into());
        plan.add_seed(seed("alpha", 0.42));
        let prompt = build_rerank_prompt(&plan);
        assert!(prompt.contains("router.dart"));
        assert!(prompt.contains("alpha"));
        assert!(prompt.contains("\"items\""));
    }
}
