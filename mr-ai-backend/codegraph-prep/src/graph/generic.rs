//! Python linker (baseline).
//!
//! Maps `Import`-kind AST nodes to file-level `Imports` edges where possible.

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

/// Build a simple, language-agnostic graph. No IO required.
///
/// Heuristics:
/// - Add every AST node as a graph node;
/// - `Declares`: file → any declaration in the same file (excludes file/import/export/part/part_of);
/// - `SameFile`: between all nodes that share the same file (directed both ways);
/// - `Imports` / `Exports` / `Part`: only **file → file** edges:
///     * if `resolved_target` is present → use it;
///     * else try a best-effort guess by comparing the last path segment of `name` to file paths.
pub fn build(
    _root: &Path,
    nodes: &[AstNode],
    _cfg: &GraphConfig,
) -> anyhow::Result<Graph<AstNode, GraphEdgeLabel>> {
    let mut g: Graph<AstNode, GraphEdgeLabel> = Graph::new();

    // 1) Add all nodes and keep simple indices.
    let mut idx_by_sym: HashMap<String, _> = HashMap::new();
    for n in nodes {
        let i = g.add_node(n.clone());
        idx_by_sym.insert(n.symbol_id.clone(), i);
    }

    // 2) Collect file-node indices by path for fast file→file linking.
    let mut file_idx_by_path: HashMap<String, _> = HashMap::new();
    for i in g.node_indices() {
        if matches!(g[i].kind, AstKind::File) {
            file_idx_by_path.insert(g[i].file.clone(), i);
        }
    }

    // 3) Declares: file → decl (exclude file/import/export/part/part_of).
    for fidx in g.node_indices() {
        if !matches!(g[fidx].kind, AstKind::File) {
            continue;
        }
        let file_path = g[fidx].file.clone();
        for nidx in g.node_indices() {
            if fidx == nidx {
                continue;
            }
            let n = &g[nidx];
            if n.file == file_path
                && !matches!(
                    n.kind,
                    AstKind::File
                        | AstKind::Import
                        | AstKind::Export
                        | AstKind::Part
                        | AstKind::PartOf
                )
            {
                g.add_edge(fidx, nidx, GraphEdgeLabel::Declares);
            }
        }
    }

    // 4) SameFile: connect all nodes sharing the same file (directed both ways).
    for i in g.node_indices() {
        let fi = g[i].file.clone();
        for j in g.node_indices() {
            if i == j {
                continue;
            }
            if g[j].file == fi {
                g.add_edge(i, j, GraphEdgeLabel::SameFile);
            }
        }
    }

    // 5) File-to-file Imports / Exports / Part edges.
    for nidx in g.node_indices() {
        let n = &g[nidx];
        let edge_label = match n.kind {
            AstKind::Import => Some(GraphEdgeLabel::Imports),
            AstKind::Export => Some(GraphEdgeLabel::Exports),
            AstKind::Part => Some(GraphEdgeLabel::Part),
            _ => None,
        };
        if edge_label.is_none() {
            continue;
        }
        // source file node
        let Some(&src_file_idx) = file_idx_by_path.get(&n.file) else {
            continue;
        };

        // determine destination file path
        let dst_path = if let Some(rt) = &n.resolved_target {
            Some(rt.clone())
        } else {
            // best-effort: match by the last segment of `name`
            let guess = n.name.split('/').last().unwrap_or(&n.name);
            // find any file node whose path ends with that segment
            let mut found: Option<String> = None;
            for (fp, _) in &file_idx_by_path {
                if fp.ends_with(guess) {
                    found = Some(fp.clone());
                    break;
                }
            }
            found
        };

        if let Some(dst) = dst_path {
            if let Some(&dst_file_idx) = file_idx_by_path.get(&dst) {
                g.add_edge(src_file_idx, dst_file_idx, edge_label.unwrap());
            }
        }
    }

    debug!(
        "generic linker: nodes={}, edges={}",
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
