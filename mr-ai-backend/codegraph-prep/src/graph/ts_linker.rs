//! TypeScript/JavaScript linker.
//!
//! This builder creates file-level import/export edges based on `Import`/`Export` AST nodes.
//! Path resolution heuristics (baseline):
//! - If `resolved_target` is present, use it;
//! - Otherwise, if `name` starts with `.` or `..`, resolve relative to the source file;
//! - Try common TS index resolution: append `/index.ts` or `/index.tsx` if the path points to a folder;
//! - No tsconfig `paths` aliasing here (kept minimal); this can be extended later.

use crate::{
    config::model::GraphConfig,
    model::{
        ast::{AstKind, AstNode},
        graph::GraphEdgeLabel,
    },
};
use petgraph::graph::{Graph, NodeIndex};
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

pub fn build(
    _root: &Path,
    nodes: &[AstNode],
    _cfg: &GraphConfig,
) -> anyhow::Result<Graph<AstNode, GraphEdgeLabel>> {
    let mut g: Graph<AstNode, GraphEdgeLabel> = Graph::new();
    let mut idx_by_sym: HashMap<String, NodeIndex> = HashMap::new();

    for n in nodes {
        let idx = g.add_node(n.clone());
        idx_by_sym.insert(n.symbol_id.clone(), idx);
    }

    // file nodes by path
    let mut file_idx: HashMap<String, NodeIndex> = HashMap::new();
    for i in g.node_indices() {
        if matches!(g[i].kind, AstKind::File) {
            file_idx.insert(g[i].file.clone(), i);
        }
    }

    // peers declared in file
    for i in g.node_indices() {
        if matches!(g[i].kind, AstKind::File) {
            let path = g[i].file.clone();
            for j in g.node_indices() {
                if i != j
                    && g[j].file == path
                    && !matches!(g[j].kind, AstKind::File | AstKind::Import | AstKind::Export)
                {
                    g.add_edge(i, j, GraphEdgeLabel::Declares);
                }
            }
        }
    }

    // imports/exports
    for i in g.node_indices() {
        let n = &g[i];
        if !(matches!(n.kind, AstKind::Import | AstKind::Export)) {
            continue;
        }

        if let Some(dst_key) = resolve_ts_like(&n.file, &n.name, &n.resolved_target) {
            if let (Some(&srcf), Some(&dstf)) = (file_idx.get(&n.file), file_idx.get(&dst_key)) {
                let label = if matches!(n.kind, AstKind::Import) {
                    GraphEdgeLabel::Imports
                } else {
                    GraphEdgeLabel::Exports
                };
                g.add_edge(srcf, dstf, label);
            }
        }
    }

    debug!(
        "ts linker: nodes={}, edges={}",
        g.node_count(),
        g.edge_count()
    );
    Ok(g)
}

fn resolve_ts_like(src_file: &str, spec: &str, resolved: &Option<String>) -> Option<String> {
    if let Some(r) = resolved {
        return Some(r.clone());
    }
    if spec.starts_with('.') {
        let base = Path::new(src_file).parent().unwrap_or(Path::new(""));
        let p = base.join(spec);
        if p.extension().is_none() {
            // try common TS/JS endings
            for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
                let candidate = p.with_extension(ext);
                if candidate.exists() {
                    return Some(candidate.to_string_lossy().to_string());
                }
            }
            // folder index
            for index in ["index.ts", "index.tsx", "index.js", "index.jsx"] {
                let candidate = p.join(index);
                if candidate.exists() {
                    return Some(candidate.to_string_lossy().to_string());
                }
            }
        }
        return Some(p.to_string_lossy().to_string());
    }
    None
}
