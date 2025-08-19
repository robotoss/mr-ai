//! JSONL writers for pipeline artifacts.
//!
//! Each writer outputs **one compact JSON object per line**, making the format
//! grep-friendly and easy to stream. The format is stable across runs; avoid
//! breaking changes unless versioned explicitly.

use crate::{
    core::normalize::normalize_repo_rel_str,
    model::{ast::AstNode, graph::GraphEdgeLabel},
};
use anyhow::{Context, Result};
use petgraph::graph::Graph;
use serde_json::json;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
use tracing::info;

/// Write [`AstNode`]s as JSON Lines (`ast_nodes.jsonl`).
///
/// Nodes are serialized directly via [`serde`], one per line.
///
/// # Example
/// ```no_run
/// use std::path::Path;
/// use codegraph_prep::export::jsonl::write_ast_nodes_jsonl;
/// use codegraph_prep::model::ast::AstNode;
///
/// let nodes: Vec<AstNode> = vec![];
/// write_ast_nodes_jsonl(Path::new("ast_nodes.jsonl"), &nodes).unwrap();
/// ```
pub fn write_ast_nodes_jsonl(path: &Path, nodes: &[AstNode]) -> Result<()> {
    let f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut w = BufWriter::new(f);

    for n in nodes {
        serde_json::to_writer(&mut w, n)?;
        w.write_all(b"\n")?;
    }

    w.flush()?;
    info!("jsonl: wrote AST nodes -> {}", path.display());
    Ok(())
}

/// Write graph nodes/edges as JSONL (`graph_nodes.jsonl` + `graph_edges.jsonl`).
///
/// Nodes are flattened into the form:
/// ```json
/// { "id": 0, "name": "main", "type": "function", "file": "code_data/project_x/lib/foo.dart", "start_line": 10, "end_line": 20 }
/// ```
///
/// Edges are emitted as:
/// ```json
/// { "src": 0, "dst": 1, "label": "imports" }
/// ```
///
/// IDs are stable within a single run (0..N-1) and consistent between nodes
/// and edges.
///
/// # Arguments
/// * `nodes_path` – where to write graph nodes
/// * `edges_path` – where to write graph edges
/// * `graph` – the in-memory graph
/// * `root` – repository root, used for path normalization
pub fn write_graph_jsonl(
    nodes_path: &Path,
    edges_path: &Path,
    graph: &Graph<AstNode, GraphEdgeLabel>,
    root: &Path,
) -> Result<()> {
    // Build stable mapping: NodeIndex -> ordinal ID
    let mut id_map = Vec::with_capacity(graph.node_count());
    for (i, nidx) in graph.node_indices().enumerate() {
        if nidx.index() >= id_map.len() {
            id_map.resize(nidx.index() + 1, usize::MAX);
        }
        id_map[nidx.index()] = i;
    }

    // --- Write nodes ---
    {
        let f =
            File::create(nodes_path).with_context(|| format!("create {}", nodes_path.display()))?;
        let mut w = BufWriter::new(f);

        for (i, nidx) in graph.node_indices().enumerate() {
            let n = &graph[nidx];

            // Normalize paths
            let norm_file = normalize_repo_rel_str(root, Path::new(&n.file));
            let norm_name = if n.kind == crate::model::ast::AstKind::File {
                norm_file.clone()
            } else {
                n.name.clone()
            };

            let rec = json!({
                "id": i,
                "name": norm_name,
                "type": ast_kind_key(&n.kind),
                "file": norm_file,
                "start_line": n.span.start_line,
                "end_line": n.span.end_line,
            });
            serde_json::to_writer(&mut w, &rec)?;
            w.write_all(b"\n")?;
        }

        w.flush()?;
        info!("jsonl: wrote graph nodes -> {}", nodes_path.display());
    }

    // --- Write edges ---
    {
        let f =
            File::create(edges_path).with_context(|| format!("create {}", edges_path.display()))?;
        let mut w = BufWriter::new(f);

        for eidx in graph.edge_indices() {
            if let Some((s, d)) = graph.edge_endpoints(eidx) {
                let src = id_map[s.index()];
                let dst = id_map[d.index()];
                let rec = json!({
                    "src": src,
                    "dst": dst,
                    "label": graph[eidx].to_string(),
                });
                serde_json::to_writer(&mut w, &rec)?;
                w.write_all(b"\n")?;
            }
        }

        w.flush()?;
        info!("jsonl: wrote graph edges -> {}", edges_path.display());
    }

    Ok(())
}

// --- helpers ---

fn ast_kind_key(kind: &crate::model::ast::AstKind) -> &'static str {
    use crate::model::ast::AstKind::*;
    match kind {
        File => "file",
        Module => "module",
        Package => "package",
        Class => "class",
        Mixin => "mixin",
        Enum => "enum",
        Extension => "extension",
        ExtensionType => "extension_type",
        Interface => "interface",
        TypeAlias => "type_alias",
        Trait => "trait",
        Impl => "impl",
        Function => "function",
        Method => "method",
        Field => "field",
        Variable => "variable",
        Import => "import",
        Export => "export",
        Part => "part",
        PartOf => "part_of",
        Macro => "macro",
    }
}
