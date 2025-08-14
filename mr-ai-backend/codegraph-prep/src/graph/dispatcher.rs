//! Graph dispatcher: builds language-aware dependency graphs.
//!
//! Combines per-language graph builders into a unified project graph.

use crate::{config::model::GraphConfig, model::ast::AstNode};
use anyhow::Result;
use petgraph::Graph;

/// Build a unified language-aware graph from AST nodes.
#[tracing::instrument(level = "info", skip_all)]
pub fn build_language_aware_graph(
    nodes: &[AstNode],
    _config: &GraphConfig,
) -> Result<Graph<AstNode, ()>> {
    let mut graph = Graph::new();
    for n in nodes {
        graph.add_node(n.clone());
    }
    Ok(graph)
}
