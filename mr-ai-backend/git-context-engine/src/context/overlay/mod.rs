//! In-memory transient overlay used by the MR review pipeline.
//!
//! Stable code lives in Qdrant + Postgres `graph_*` tables (S3+). MR-time
//! changes never touch those — instead we build an `OverlayGraph` per
//! review run that holds:
//! - The new/edited chunks parsed from the worktree at the MR head ref.
//! - The incremental edges those chunks add (or, conceptually, *replace*).
//! - The set of stable graph node IDs reachable within `max_hops` from the
//!   touched chunks (looked up via [`persistence::repos::graph::expand_k_hops`]).
//!
//! Retrieval (S4) merges the overlay with stable Qdrant/graph results just
//! before the LLM rerank step. The overlay is dropped after the review.

pub mod build;
pub mod merge;

pub use build::{build_for_mr, plan_walk, OverlayBuildReport, OverlayCaps, WalkPlan};
pub use merge::OverlayEmbedCache;

use std::collections::{BTreeMap, BTreeSet};

use code_indexer::CodeChunk;
use domain::NodeId;

/// Per-MR transient view layered on top of the stable index.
#[derive(Debug, Clone, Default)]
pub struct OverlayGraph {
    /// Chunks produced from the MR worktree (changed files only). Keyed by
    /// `CodeChunk.id` for stable lookup.
    pub new_chunks: BTreeMap<String, CodeChunk>,
    /// Stable graph nodes pulled in via k-hop expansion from the changed
    /// files. Persistence stays read-only.
    pub neighbour_nodes: BTreeSet<NodeId>,
    /// Repo-relative file paths that the overlay considers "touched".
    pub touched_files: BTreeSet<String>,
}

impl OverlayGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a file as touched and stage all chunks that came from it.
    pub fn ingest_chunks(&mut self, chunks: impl IntoIterator<Item = CodeChunk>) {
        for chunk in chunks {
            self.touched_files.insert(chunk.file.clone());
            self.new_chunks.insert(chunk.id.clone(), chunk);
        }
    }

    /// Add a stable node id discovered during graph expansion.
    pub fn record_neighbour(&mut self, id: NodeId) {
        self.neighbour_nodes.insert(id);
    }

    pub fn record_neighbours(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        for id in ids {
            self.neighbour_nodes.insert(id);
        }
    }

    pub fn chunk_count(&self) -> usize {
        self.new_chunks.len()
    }

    pub fn neighbour_count(&self) -> usize {
        self.neighbour_nodes.len()
    }

    pub fn touched_count(&self) -> usize {
        self.touched_files.len()
    }

    /// Return chunks belonging to a given file. Used by the retriever to
    /// prefer overlay matches over stable ones for the same path.
    pub fn chunks_for_file(&self, file: &str) -> Vec<&CodeChunk> {
        self.new_chunks
            .values()
            .filter(|c| c.file == file)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_indexer::types::{ChunkFeatures, LanguageKind, Span, SymbolKind};

    fn dummy_chunk(file: &str, symbol: &str) -> CodeChunk {
        CodeChunk {
            id: format!("{file}#{symbol}"),
            language: LanguageKind::Dart,
            file: file.into(),
            symbol: symbol.into(),
            symbol_path: format!("{file}::{symbol}"),
            kind: SymbolKind::Method,
            span: Span {
                start_byte: 0,
                end_byte: 1,
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: 1,
            },
            owner_path: vec![],
            doc: None,
            annotations: vec![],
            imports: vec![],
            signature: None,
            is_definition: true,
            is_generated: false,
            snippet: None,
            features: ChunkFeatures::default(),
            content_sha256: "x".into(),
            neighbors: None,
            identifiers: vec![],
            anchors: vec![],
            graph: None,
            hints: None,
            lsp: None,
            extras: None,
            parent_symbol_id: None,
            chunk_kind: None,
        }
    }

    #[test]
    fn ingest_collects_chunks_and_files() {
        let mut overlay = OverlayGraph::new();
        overlay.ingest_chunks(vec![
            dummy_chunk("lib/a.dart", "x"),
            dummy_chunk("lib/a.dart", "y"),
            dummy_chunk("lib/b.dart", "z"),
        ]);
        assert_eq!(overlay.chunk_count(), 3);
        assert_eq!(overlay.touched_count(), 2);
        assert_eq!(overlay.chunks_for_file("lib/a.dart").len(), 2);
    }

    #[test]
    fn neighbours_dedup() {
        let mut overlay = OverlayGraph::new();
        let n = NodeId::new();
        overlay.record_neighbour(n);
        overlay.record_neighbour(n);
        assert_eq!(overlay.neighbour_count(), 1);
    }
}
