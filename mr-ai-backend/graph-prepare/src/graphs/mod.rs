//! Language-aware graph builders and dispatcher.

pub mod dart_graph;
pub mod edge;
pub mod generic_graph;
pub mod merge;

use crate::graphs::dart_graph::build_graph_dart;
use crate::graphs::edge::GraphEdge;
use crate::graphs::generic_graph::build_graph_generic;
use crate::graphs::merge::merge_graphs;
use crate::models::ast_node::ASTNode;
use anyhow::Result;
use petgraph::graph::Graph;

/// Heuristically bucketize nodes by file extension.
pub fn bucketize(nodes: &[ASTNode]) -> (Vec<ASTNode>, Vec<ASTNode>) {
    let mut dart = Vec::new();
    let mut other = Vec::new();
    for n in nodes {
        if n.file.ends_with(".dart") {
            dart.push(n.clone());
        } else {
            other.push(n.clone());
        }
    }
    (dart, other)
}

/// Dispatch build per language and merge graphs into a single graph.
///
/// - Dart nodes are passed to `build_graph_dart(root, ...)`, which builds a file-level graph
///   with `imports/exports/part` and `declares` edges.
/// - All other nodes are passed to `build_graph_generic(...)` as a fallback.
pub fn build_graph_language_aware(
    root: &str,
    nodes: &[ASTNode],
) -> Result<Graph<ASTNode, GraphEdge>> {
    let (dart_nodes, other_nodes) = bucketize(nodes);

    let mut graphs = Vec::<Graph<ASTNode, GraphEdge>>::new();

    if !dart_nodes.is_empty() {
        graphs.push(build_graph_dart(root, &dart_nodes)?);
    }
    if !other_nodes.is_empty() {
        graphs.push(build_graph_generic(&other_nodes));
    }

    // Merge all subgraphs into one
    let merged = merge_graphs(graphs);
    Ok(merged)
}
