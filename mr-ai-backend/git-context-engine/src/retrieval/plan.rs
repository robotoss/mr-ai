//! Pure-data plan describing one retrieval run.
//!
//! Construction: a worker resolves a `ReviewTarget` to one or more seeds
//! (vector hits, exact symbol matches, overlay anchors) and folds them into
//! a `RetrievalPlan`. The plan is then handed to a reranker. Keeping the
//! plan pure makes the pipeline easy to unit-test and to swap rerankers
//! without rewiring upstream code.

use std::collections::BTreeMap;

use domain::{NodeId, RetrievalConfig};
use serde::{Deserialize, Serialize};

/// A single seed result before graph expansion. `score` is whatever the
/// upstream similarity layer reports (cosine for vectors, BM25-normalised
/// for lexical, `1.0` for exact matches from the overlay).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalSeed {
    pub chunk_id: String,
    pub file: String,
    pub symbol_path: String,
    pub score: f32,
    pub source: SeedSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedSource {
    Vector,
    Lexical,
    Overlay,
    Exact,
}

/// Final scored hit (post-expansion / dedup / score-floor / token budget).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredHit {
    pub chunk_id: String,
    pub file: String,
    pub symbol_path: String,
    pub score: f32,
    pub via: SeedSource,
    /// Hops away from the seed cluster. `0` = seed itself, `n` = reached
    /// after `n` graph edges.
    pub hops: u8,
}

/// Pure description of one retrieval run. Builders elsewhere fill it in;
/// the reranker turns it into the final `Vec<ScoredHit>`.
#[derive(Debug, Clone, Default)]
pub struct RetrievalPlan {
    pub config: RetrievalConfig,
    pub seeds: Vec<RetrievalSeed>,
    /// IDs of stable graph nodes the BFS touched, with their distance.
    pub expanded: BTreeMap<NodeId, u8>,
    /// Optional query string used for diagnostics / tracing.
    pub query: Option<String>,
}

impl RetrievalPlan {
    pub fn new(config: RetrievalConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    pub fn add_seed(&mut self, seed: RetrievalSeed) {
        self.seeds.push(seed);
    }

    pub fn add_seeds(&mut self, seeds: impl IntoIterator<Item = RetrievalSeed>) {
        for seed in seeds {
            self.seeds.push(seed);
        }
    }

    /// Record graph expansion output. `hops` must be >= 1 (seeds are 0).
    pub fn record_expansion(&mut self, ids: impl IntoIterator<Item = NodeId>, hops: u8) {
        for id in ids {
            self.expanded
                .entry(id)
                .and_modify(|cur| {
                    if hops < *cur {
                        *cur = hops;
                    }
                })
                .or_insert(hops);
        }
    }

    /// Drop seeds whose score falls below `config.min_score`. Returns the
    /// number of seeds removed for diagnostics.
    pub fn apply_score_floor(&mut self) -> usize {
        let before = self.seeds.len();
        self.seeds.retain(|s| s.score >= self.config.min_score);
        before - self.seeds.len()
    }

    /// Trim the plan so its character footprint fits the budget. Operates
    /// on `chunk_id` length as a proxy — real implementations replace this
    /// with token-counting once the rerank LLM is wired.
    pub fn enforce_token_budget(&mut self, char_per_seed: usize) -> usize {
        if char_per_seed == 0 {
            return 0;
        }
        let max_seeds = self.config.token_budget / char_per_seed;
        if self.seeds.len() > max_seeds {
            let dropped = self.seeds.len() - max_seeds;
            self.seeds.truncate(max_seeds);
            dropped
        } else {
            0
        }
    }
}

/// Cheap fallback reranker: stable-sort by score (desc), break ties by
/// `hops` (asc), then by `file`. Used in tests and as the default until
/// the LLM rerank lands in S4-B.
pub fn heuristic_rerank(plan: &RetrievalPlan) -> Vec<ScoredHit> {
    let mut hits: Vec<ScoredHit> = plan
        .seeds
        .iter()
        .map(|s| ScoredHit {
            chunk_id: s.chunk_id.clone(),
            file: s.file.clone(),
            symbol_path: s.symbol_path.clone(),
            score: s.score,
            via: s.source,
            hops: 0,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.hops.cmp(&b.hops))
            .then(a.file.cmp(&b.file))
    });
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(id: &str, score: f32, src: SeedSource) -> RetrievalSeed {
        RetrievalSeed {
            chunk_id: id.into(),
            file: format!("{id}.dart"),
            symbol_path: format!("{id}::doit"),
            score,
            source: src,
        }
    }

    #[test]
    fn score_floor_drops_low_signal_seeds() {
        let mut plan = RetrievalPlan::new(RetrievalConfig {
            min_score: 0.5,
            ..RetrievalConfig::DEFAULT
        });
        plan.add_seeds(vec![
            seed("hi", 0.9, SeedSource::Vector),
            seed("mid", 0.5, SeedSource::Vector),
            seed("lo", 0.2, SeedSource::Lexical),
        ]);
        let dropped = plan.apply_score_floor();
        assert_eq!(dropped, 1);
        assert_eq!(plan.seeds.len(), 2);
    }

    #[test]
    fn token_budget_truncates_to_estimate() {
        let mut plan = RetrievalPlan::new(RetrievalConfig {
            token_budget: 30, // 30 chars
            ..RetrievalConfig::DEFAULT
        });
        plan.add_seeds(vec![
            seed("a", 1.0, SeedSource::Vector),
            seed("b", 0.9, SeedSource::Vector),
            seed("c", 0.8, SeedSource::Vector),
            seed("d", 0.7, SeedSource::Vector),
            seed("e", 0.6, SeedSource::Vector),
        ]);
        // 10 chars per seed budget => keep 3.
        let dropped = plan.enforce_token_budget(10);
        assert_eq!(dropped, 2);
        assert_eq!(plan.seeds.len(), 3);
    }

    #[test]
    fn record_expansion_keeps_min_hops() {
        let mut plan = RetrievalPlan::new(RetrievalConfig::default());
        let n = NodeId::new();
        plan.record_expansion([n], 2);
        plan.record_expansion([n], 1);
        plan.record_expansion([n], 3);
        assert_eq!(plan.expanded.get(&n), Some(&1));
    }

    #[test]
    fn heuristic_rerank_orders_by_score_then_file() {
        let mut plan = RetrievalPlan::new(RetrievalConfig::default());
        plan.add_seeds(vec![
            seed("z", 0.5, SeedSource::Vector),
            seed("a", 0.9, SeedSource::Vector),
            seed("m", 0.5, SeedSource::Lexical),
        ]);
        let hits = heuristic_rerank(&plan);
        assert_eq!(hits[0].chunk_id, "a");
        assert_eq!(hits[1].chunk_id, "m");
        assert_eq!(hits[2].chunk_id, "z");
    }
}
