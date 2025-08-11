use graph_prepare::{graphs::edge::GraphEdge, models::ast_node::ASTNode};
use petgraph::{graph::Graph, visit::EdgeRef};

use crate::chunker::Chunk;

/// Return a small list of neighbor identifiers for a chunk (e.g., imported files).
pub fn neighbors_for_chunk(g: &Graph<ASTNode, GraphEdge>, ch: &Chunk) -> Vec<String> {
    let mut acc = Vec::new();

    let file_idx = g
        .node_indices()
        .find(|&ni| g[ni].node_type == "file" && g[ni].file == ch.file);
    if let Some(fi) = file_idx {
        if ch.node_type == "file" {
            for ei in g.edges(fi) {
                let lab = &ei.weight().0;
                if lab == "imports"
                    || lab == "exports"
                    || lab == "imports_via_export"
                    || lab == "part"
                {
                    let dst = ei.target();
                    if g[dst].node_type == "file" {
                        acc.push(g[dst].file.clone());
                    }
                }
            }
        } else {
            acc.push(ch.file.clone());
            for ei in g.edges(fi) {
                let lab = &ei.weight().0;
                if lab == "imports"
                    || lab == "exports"
                    || lab == "imports_via_export"
                    || lab == "part"
                {
                    let dst = ei.target();
                    if g[dst].node_type == "file" {
                        acc.push(g[dst].file.clone());
                    }
                }
            }
        }
    }
    acc
}
