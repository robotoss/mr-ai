//! Rust linker.
//!
//! Minimal linking rules (baseline):
//! - `Declares`: file → {struct/enum/trait/impl/fn/const/static/...} in the same file;
//! - `Import` edges from `use` declarations are mapped to `Import` kind in AST (if extractor does so);
//! - Path resolution is not attempted here (crates/mod system is richer); can be extended later.

use crate::{
    config::model::GraphConfig,
    model::{
        ast::{AstKind, AstNode},
        graph::GraphEdgeLabel,
    },
};
use petgraph::graph::Graph;
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

pub fn build(
    _root: &Path,
    nodes: &[AstNode],
    _cfg: &GraphConfig,
) -> anyhow::Result<Graph<AstNode, GraphEdgeLabel>> {
    let mut g: Graph<AstNode, GraphEdgeLabel> = Graph::new();

    let mut by_sym = HashMap::new();
    for n in nodes {
        let i = g.add_node(n.clone());
        by_sym.insert(n.symbol_id.clone(), i);
    }

    let mut file_idx = HashMap::new();
    for i in g.node_indices() {
        if matches!(g[i].kind, AstKind::File) {
            file_idx.insert(g[i].file.clone(), i);
        }
    }

    // declares
    for i in g.node_indices() {
        if matches!(g[i].kind, AstKind::File) {
            let fp = g[i].file.clone();
            for j in g.node_indices() {
                if i != j
                    && g[j].file == fp
                    && !matches!(g[j].kind, AstKind::File | AstKind::Import)
                {
                    g.add_edge(i, j, GraphEdgeLabel::Declares);
                }
            }
        }
    }

    // (optional) same_file could be useful for neighborhood
    for i in g.node_indices() {
        let fp = g[i].file.clone();
        for j in g.node_indices() {
            if i != j && g[j].file == fp {
                g.add_edge(i, j, GraphEdgeLabel::SameFile);
            }
        }
    }

    debug!(
        "rs linker: nodes={}, edges={}",
        g.node_count(),
        g.edge_count()
    );
    Ok(g)
}
