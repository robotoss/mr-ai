//! Glue between an `LlmReviewRequest` and the LLM rerank pipeline.
//!
//! `build_two_phase_review` already turns a diff bundle into per-hunk
//! `LlmReviewTarget`s with rendered prompts. The functions here lift that
//! shape into a `RetrievalPlan` so the Smart-tier rerank can decide which
//! hunks deserve the most attention from the reviewer LLM. The rerank
//! result is intended to be persisted into `mr_reviews.bundle` for
//! diagnostics and to be used as a sort key when the reviewer publishes
//! comments under a budget.

use std::sync::Arc;
use std::time::Duration;

use ai_llm_service::LlmGateway;
use domain::RetrievalConfig;

use crate::review::prompt::LlmReviewRequest;
use crate::review::retrieval::llm_rerank::llm_rerank;
use crate::review::retrieval::plan::{
    heuristic_rerank, RetrievalPlan, RetrievalSeed, ScoredHit, SeedSource,
};

/// Build a `RetrievalPlan` whose seeds correspond to `LlmReviewRequest`'s
/// targets. Each target becomes one seed with `source = Vector` and a
/// score that reflects the `planned_anchors` priority — `High` → 0.9,
/// `Medium` → 0.6, `Low` → 0.4, missing/other → 0.5.
pub fn plan_from_review_request(
    request: &LlmReviewRequest,
    config: RetrievalConfig,
) -> RetrievalPlan {
    let mut plan = RetrievalPlan::new(config);
    plan.query = Some(format!(
        "Review hunks from {}#{}",
        request.change.project, request.change.iid
    ));
    for (idx, target) in request.targets.iter().enumerate() {
        let seed_score = priority_to_score(&target.planned_anchors);
        plan.add_seed(RetrievalSeed {
            chunk_id: format!("{}#{}", target.file_path, target.hunk_index),
            file: target.file_path.clone(),
            symbol_path: format!("{}::hunk_{}", target.file_path, idx),
            score: seed_score,
            source: SeedSource::Vector,
        });
    }
    plan
}

/// Run the LLM rerank against the supplied review request. Returns a
/// list of `ScoredHit`s in the same shape as `llm_rerank` — callers
/// match by `chunk_id` (`<file_path>#<hunk_index>`) to reorder targets.
///
/// On any failure (timeout, gateway error, bad payload) the function
/// transparently degrades to `heuristic_rerank` so the caller always
/// gets a usable ordering.
pub async fn rerank_review_request(
    gateway: Arc<LlmGateway>,
    request: &LlmReviewRequest,
    config: RetrievalConfig,
    timeout: Duration,
) -> Vec<ScoredHit> {
    let plan = plan_from_review_request(request, config);
    if plan.seeds.is_empty() {
        return Vec::new();
    }
    let hits = llm_rerank(gateway, &plan, timeout).await;
    if hits.is_empty() {
        // llm_rerank itself only returns empty on empty seeds; treat any
        // post-processing wipe-out as a fallback signal.
        heuristic_rerank(&plan)
    } else {
        hits
    }
}

fn priority_to_score(anchors: &[crate::review::prompt::LlmPlannedAnchor]) -> f32 {
    if anchors.is_empty() {
        return 0.5;
    }
    let mut best: f32 = 0.0;
    for a in anchors {
        let score = match a.priority.as_str() {
            "High" => 0.9,
            "Medium" => 0.6,
            "Low" => 0.4,
            _ => 0.5,
        };
        if score > best {
            best = score;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::prompt::{LlmPlannedAnchor, LlmReviewChangeMeta, LlmReviewTarget};

    fn meta() -> LlmReviewChangeMeta {
        LlmReviewChangeMeta {
            provider: "GitLab".into(),
            project: "org/app".into(),
            iid: 42,
            title: "T".into(),
            description: String::new(),
            author_name: String::new(),
            web_url: "https://x".into(),
            gitlab_head_sha: "h".into(),
            gitlab_base_sha: "b".into(),
            gitlab_start_sha: None,
        }
    }

    fn target(file: &str, hunk: usize, priority: Option<&str>) -> LlmReviewTarget {
        let mut planned = vec![];
        if let Some(p) = priority {
            planned.push(LlmPlannedAnchor {
                hypothesis_id: "H1".into(),
                priority: p.into(),
                kind: "PossibleBug".into(),
                start_line: 10,
                end_line: 12,
                anchor_lines: vec![],
            });
        }
        LlmReviewTarget {
            file_path: file.into(),
            hunk_index: hunk,
            prompt_text: format!("review {file}#{hunk}"),
            planned_anchors: planned,
        }
    }

    #[test]
    fn plan_emits_one_seed_per_target() {
        let req = LlmReviewRequest {
            change: meta(),
            targets: vec![
                target("lib/a.dart", 0, Some("High")),
                target("lib/b.dart", 1, Some("Low")),
                target("lib/c.dart", 0, None),
            ],
        };
        let plan = plan_from_review_request(&req, RetrievalConfig::default());
        assert_eq!(plan.seeds.len(), 3);
        assert!(plan.query.as_deref().unwrap().contains("org/app"));
        assert!((plan.seeds[0].score - 0.9).abs() < 1e-6);
        assert!((plan.seeds[1].score - 0.4).abs() < 1e-6);
        assert!((plan.seeds[2].score - 0.5).abs() < 1e-6);
        assert_eq!(plan.seeds[0].chunk_id, "lib/a.dart#0");
        assert!(plan.seeds.iter().all(|s| matches!(s.source, SeedSource::Vector)));
    }

    #[test]
    fn empty_request_yields_empty_plan() {
        let req = LlmReviewRequest {
            change: meta(),
            targets: vec![],
        };
        let plan = plan_from_review_request(&req, RetrievalConfig::default());
        assert!(plan.seeds.is_empty());
    }

    #[test]
    fn priority_scoring_picks_highest_anchor() {
        let mut t = target("x", 0, Some("Low"));
        t.planned_anchors.push(LlmPlannedAnchor {
            hypothesis_id: "H2".into(),
            priority: "High".into(),
            kind: "PossibleBug".into(),
            start_line: 1,
            end_line: 2,
            anchor_lines: vec![],
        });
        let req = LlmReviewRequest {
            change: meta(),
            targets: vec![t],
        };
        let plan = plan_from_review_request(&req, RetrievalConfig::default());
        assert!((plan.seeds[0].score - 0.9).abs() < 1e-6);
    }
}
