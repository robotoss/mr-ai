//! Common traits used across the pipeline (extensible).
//!
//! These traits decouple language extraction, URI resolution, and graph building,
//! making the pipeline easier to test and evolve.

use crate::model::ast::AstNode;
use std::path::Path;

/// Accepts AST nodes produced during extraction.
pub trait AstSink {
    /// Called for every `AstNode` that extractor wants to emit.
    fn on_node(&mut self, node: AstNode);
}

/// Minimal facade for a language extractor implementation.
pub trait LanguageExtractor {
    /// Extract AST nodes from (`tree`, `code`, `path`) and write them into `sink`.
    fn extract_file(&self, code: &str, path: &Path, sink: &mut dyn AstSink) -> anyhow::Result<()>;
}

/// Generic URI resolver (language-specific implementations may exist).
pub trait UriResolver {
    /// Resolve `uri` as seen from `src_file` (absolute or repo-relative) into a repo path.
    fn resolve(&self, src_file: &str, uri: &str) -> Option<std::path::PathBuf>;
}

/// GraphBuilder abstracts a stage that takes a set of `AstNode`s and returns a graph.
/// Concrete implementations would live under `graph/`.
pub trait GraphBuilder<N = AstNode, E = ()> {
    type Graph;
    fn build(&self, nodes: &[N]) -> anyhow::Result<Self::Graph>;
}
