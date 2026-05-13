//! Versioned prompt template identifiers. Sprint 4c of 🅰.
//!
//! Each LLM-facing prompt builder in the workspace is bound to one
//! `PromptId`. The id propagates into `UnifiedRequest::prompt_id`,
//! lands in `UsageRecord.prompt_id`, and appears on the usage JSONL
//! row as a stable `Name@Version` string. That gives us A/B telemetry
//! without an additional table: a Prometheus or SQL query against the
//! recorder can pivot cost / latency / outcome by prompt template.
//!
//! When a prompt template changes meaningfully (different output
//! format, new sections, schema bump) bump the version *and* keep the
//! old variant around at least one release so historical telemetry
//! still maps cleanly. Old variants land in `out of date` and can be
//! removed in a later cleanup pass.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable identifiers for every prompt template in the workspace.
///
/// The compile-time enum guarantees we can't reference a non-existent
/// template; the embedded version string lets templates evolve without
/// renaming the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PromptId {
    /// Final review prompt: full diff + RAG + rules → comments.
    ///
    /// Owner: `git_context_engine::review::prompt::builder`.
    ReviewMain,
    /// Pre-review hypothesis planner (Fast-tier).
    ///
    /// Owner: `git_context_engine::review::pre_review::builder`.
    PreReview,
    /// LLM-as-judge rerank prompt over retrieval seeds.
    ///
    /// Owner: `git_context_engine::review::retrieval::llm_rerank`.
    Rerank,
    /// Per-hypothesis review (sprint 4b).
    ///
    /// Owner: `git_context_engine::review::prompt::per_hypothesis`.
    PerHypothesis,
}

impl PromptId {
    /// Short, stable, lowercase name for telemetry. Pair with
    /// [`Self::version`] when you need a unique label per emission.
    pub fn name(&self) -> &'static str {
        match self {
            Self::ReviewMain => "review_main",
            Self::PreReview => "pre_review",
            Self::Rerank => "rerank",
            Self::PerHypothesis => "per_hypothesis",
        }
    }

    /// Semantic version of the template. Bump when the prompt text
    /// changes in a way that could affect outputs (added section,
    /// changed schema, new instructions). Patch-level prompt tweaks
    /// (typo fixes, whitespace) don't require a bump.
    pub fn version(&self) -> &'static str {
        match self {
            // Initial baseline — all templates start at v1 in sprint 4c.
            Self::ReviewMain => "v1",
            Self::PreReview => "v1",
            Self::Rerank => "v1",
            Self::PerHypothesis => "v1",
        }
    }

    /// `"name@version"` — the canonical label for telemetry rows.
    pub fn label(&self) -> String {
        format!("{}@{}", self.name(), self.version())
    }
}

impl fmt::Display for PromptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name(), self.version())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_matches_display_and_round_trips_through_serde() {
        for id in [
            PromptId::ReviewMain,
            PromptId::PreReview,
            PromptId::Rerank,
            PromptId::PerHypothesis,
        ] {
            let label = id.label();
            assert_eq!(label, id.to_string());
            assert!(label.contains('@'));
            let json = serde_json::to_string(&id).unwrap();
            let back: PromptId = serde_json::from_str(&json).unwrap();
            assert_eq!(back, id);
        }
    }

    #[test]
    fn names_are_unique_and_lowercase() {
        let names = [
            PromptId::ReviewMain.name(),
            PromptId::PreReview.name(),
            PromptId::Rerank.name(),
            PromptId::PerHypothesis.name(),
        ];
        let mut set = std::collections::HashSet::new();
        for n in names {
            assert!(n.chars().all(|c| !c.is_uppercase()), "{n}");
            assert!(set.insert(n), "duplicate name: {n}");
        }
    }
}
