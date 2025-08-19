//! Python linker.
//!
//! Minimal resolution:
//! - `Import` / `Export` do not really exist in Python the same way, but we map
//!   `import X` / `from X import Y` as `Import` kind in AST;
//! - If `name` looks relative (starts with `.`), join with source dir;
//! - Otherwise, leave unresolved (std/site-packages). You may enrich with a PYTHONPATH
//!   resolver later.

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

    // add all nodes
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

    // imports (very best-effort)
    for i in g.node_indices() {
        if !matches!(g[i].kind, AstKind::Import) {
            continue;
        }
        let srcf = match file_idx.get(&g[i].file) {
            Some(&idx) => idx,
            None => continue,
        };

        if let Some(dst_key) = resolve_py_like(&g[i].file, &g[i].name, &g[i].resolved_target) {
            if let Some(&dstf) = file_idx.get(&dst_key) {
                g.add_edge(srcf, dstf, GraphEdgeLabel::Imports);
            }
        }
    }

    debug!(
        "py linker: nodes={}, edges={}",
        g.node_count(),
        g.edge_count()
    );
    Ok(g)
}

fn resolve_py_like(src_file: &str, spec: &str, resolved: &Option<String>) -> Option<String> {
    if let Some(r) = resolved {
        return Some(r.clone());
    }
    if spec.starts_with('.') {
        let base = Path::new(src_file).parent().unwrap_or(Path::new(""));
        let candidate = base.join(spec.trim_start_matches('.'));
        return Some(candidate.to_string_lossy().to_string());
    }
    None
}
