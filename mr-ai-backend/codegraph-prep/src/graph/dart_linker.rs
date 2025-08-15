//! Dart-specific linker (file-level).
//!
//! Responsibilities:
//! - Create one **graph node** per `AstNode` (we keep AST granularity);
//! - Add `Declares` edges: from a file to its declarations (class/function/method/etc.);
//! - Resolve file-to-file `Import`/`Export`/`Part` edges using `resolved_target` or heuristics;
//! - Optionally add `ImportViaExport`: if `A --Import--> facade.dart` and
//!   `facade.dart --Export--> impl.dart`, then `A --ImportViaExport--> impl.dart`.
//!
//! Notes:
//! - This builder does not run YAML scans; it expects `resolved_target` to be populated
//!   by the extractor or earlier pipeline phase when possible. As a fallback, it tries
//!   relative resolution for paths like `../foo.dart`.
//! - We only connect **file nodes** for import/export/part. Declarations are connected via
//!   `Declares` from their source file.

use crate::{
    config::model::GraphConfig,
    model::{
        ast::{AstKind, AstNode},
        graph::GraphEdgeLabel,
    },
};
use petgraph::graph::{Graph, NodeIndex};
use std::{collections::HashMap, path::Path};
use tracing::debug;

const FLATTEN_EXPORTS: bool = true;

/// Build a Dart graph with file-level edges + declarations.
pub fn build(
    _root: &Path,
    nodes: &[AstNode],
    _cfg: &GraphConfig,
) -> anyhow::Result<Graph<AstNode, GraphEdgeLabel>> {
    let mut g: Graph<AstNode, GraphEdgeLabel> = Graph::new();

    // 1) Add all nodes; remember indices
    let mut idx: HashMap<String, NodeIndex> = HashMap::new(); // symbol_id → idx
    for n in nodes {
        let id = g.add_node(n.clone());
        idx.insert(n.symbol_id.clone(), id);
    }

    // 2) Collect file node indices and map by path
    let mut file_idx_by_path: HashMap<String, NodeIndex> = HashMap::new();
    for i in g.node_indices() {
        if matches!(g[i].kind, AstKind::File) {
            file_idx_by_path.insert(g[i].file.clone(), i);
        }
    }

    // 3) Declares: file → decl
    for i in g.node_indices() {
        let n = &g[i];
        if matches!(n.kind, AstKind::File) {
            let file_path = n.file.clone();
            for j in g.node_indices() {
                if i == j {
                    continue;
                }
                if g[j].file == file_path
                    && !matches!(
                        g[j].kind,
                        AstKind::File
                            | AstKind::Import
                            | AstKind::Export
                            | AstKind::Part
                            | AstKind::PartOf
                    )
                {
                    g.add_edge(i, j, GraphEdgeLabel::Declares);
                }
            }
        }
    }

    // 4) imports/exports/part between file nodes
    let mut exported_from: HashMap<NodeIndex, Vec<NodeIndex>> = HashMap::new();

    for i in g.node_indices() {
        let n = &g[i];
        match n.kind {
            AstKind::Import | AstKind::Export | AstKind::Part => {
                if let Some(dst_key) = resolve_target_guess(&n.file, &n.name, &n.resolved_target) {
                    if let Some(&dst_file_idx) = file_idx_by_path.get(&dst_key) {
                        let label = match n.kind {
                            AstKind::Import => GraphEdgeLabel::Imports,
                            AstKind::Export => GraphEdgeLabel::Exports,
                            AstKind::Part => GraphEdgeLabel::Part,
                            _ => unreachable!(),
                        };
                        // connect file-to-file: from SRC FILE to DST FILE
                        if let Some(&src_file_idx) = file_idx_by_path.get(&n.file) {
                            g.add_edge(src_file_idx, dst_file_idx, label);
                            if matches!(label, GraphEdgeLabel::Exports) {
                                exported_from
                                    .entry(src_file_idx)
                                    .or_default()
                                    .push(dst_file_idx);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // 5) Flatten re-exports
    if FLATTEN_EXPORTS {
        let mut extras: Vec<(NodeIndex, NodeIndex)> = Vec::new();

        // For each edge (src -> facade) Import, if the facade exports impls, connect src -> impl
        let mut to_scan = Vec::new();
        for e in g.edge_indices() {
            if g[e] == GraphEdgeLabel::Imports {
                if let Some((s, d)) = g.edge_endpoints(e) {
                    to_scan.push((s, d));
                }
            }
        }

        for (src, facade) in to_scan {
            if let Some(impls) = exported_from.get(&facade) {
                for &impl_idx in impls {
                    extras.push((src, impl_idx));
                }
            }
        }

        for (s, d) in extras {
            g.add_edge(s, d, GraphEdgeLabel::ImportsViaExport);
        }
    }

    debug!(
        "dart linker: nodes={}, edges={}",
        g.node_count(),
        g.edge_count()
    );
    Ok(g)
}

/// Best-effort resolution:
/// - If `resolved_target` is set, use it;
/// - Else if `name` looks like relative path (`../` or `./` or ends with `.dart`), join with src dir;
/// - Else return None.
fn resolve_target_guess(
    src_file: &str,
    uri_or_name: &str,
    resolved: &Option<String>,
) -> Option<String> {
    if let Some(r) = resolved {
        return Some(r.clone());
    }
    if uri_or_name.starts_with("../")
        || uri_or_name.starts_with("./")
        || uri_or_name.ends_with(".dart")
    {
        let base = Path::new(src_file).parent().unwrap_or(Path::new(""));
        let joined = base.join(uri_or_name);
        return Some(joined.to_string_lossy().to_string());
    }
    None
}
