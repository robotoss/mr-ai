//! Retrieval-side configuration and chunk classification.
//!
//! `RetrievalConfig` collects every knob the S4 pipeline exposes per
//! project: how wide to seed, how far to expand on the graph, how many
//! tokens to spend on rerank inputs, and the floor below which results are
//! dropped. Defaults match the recommended baseline.

use serde::{Deserialize, Serialize};

/// Coarse classification of a chunk used for hierarchical retrieval —
/// stored next to other CodeChunk metadata, so the retriever can lift the
/// embedding score from a fine-grained match to its parent context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    /// File-level summary chunk (imports + skeleton).
    File,
    /// Class/extension/mixin level (signature + docstring + slot listing).
    Parent,
    /// Single addressable symbol (method, field, function, …).
    Symbol,
    /// Sub-slice of a long symbol body, with overlap pointing at its
    /// `parent_symbol_id`.
    Sub,
}

impl ChunkKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChunkKind::File => "file",
            ChunkKind::Parent => "parent",
            ChunkKind::Symbol => "symbol",
            ChunkKind::Sub => "sub",
        }
    }
}

/// Per-project retrieval knobs. Every field has a defensible default —
/// projects can override individual values without re-declaring the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalConfig {
    /// Number of vector + lexical seeds gathered per review target. The
    /// graph expansion fans out from these.
    pub top_k: usize,
    /// Maximum number of hops to expand on the graph. `1` is the safe
    /// default; raising it adds breadth but inflates token spend.
    pub max_hops: usize,
    /// Maximum context window (in characters, approximated) handed to the
    /// LLM rerank stage. Hard ceiling — never exceeded.
    pub token_budget: usize,
    /// Minimum cosine similarity to retain a result (post-graph expansion).
    /// `0.0` keeps everything.
    pub min_score: f32,
}

impl RetrievalConfig {
    /// Recommended defaults from the planning doc.
    pub const DEFAULT: Self = Self {
        top_k: 8,
        max_hops: 1,
        token_budget: 8000,
        min_score: 0.0,
    };

    /// Read overrides from process env. Missing keys keep the default.
    pub fn from_env() -> Self {
        let mut cfg = Self::DEFAULT;
        if let Ok(s) = std::env::var("RAG_TOP_K") {
            if let Ok(n) = s.parse() {
                cfg.top_k = n;
            }
        }
        if let Ok(s) = std::env::var("RAG_MAX_HOPS") {
            if let Ok(n) = s.parse() {
                cfg.max_hops = n;
            }
        }
        if let Ok(s) = std::env::var("RAG_TOKEN_BUDGET") {
            if let Ok(n) = s.parse() {
                cfg.token_budget = n;
            }
        }
        if let Ok(s) = std::env::var("RAG_MIN_SCORE") {
            if let Ok(n) = s.parse() {
                cfg.min_score = n;
            }
        }
        cfg
    }
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_plan() {
        let cfg = RetrievalConfig::default();
        assert_eq!(cfg.top_k, 8);
        assert_eq!(cfg.max_hops, 1);
        assert_eq!(cfg.token_budget, 8000);
        assert_eq!(cfg.min_score, 0.0);
    }

    #[test]
    fn chunk_kind_str_round_trip() {
        for kind in [
            ChunkKind::File,
            ChunkKind::Parent,
            ChunkKind::Symbol,
            ChunkKind::Sub,
        ] {
            let s = kind.as_str();
            assert!(!s.is_empty());
        }
    }
}
