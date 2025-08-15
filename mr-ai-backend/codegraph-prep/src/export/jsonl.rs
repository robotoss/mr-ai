//! JSONL writers: AST nodes, graph (nodes+edges), and RAG records.
//!
//! Each function writes **one record per line** with compact JSON objects.
//! The format is stable and grep-friendly; avoid breaking changes unless versioned.

use crate::model::{ast::AstNode, graph::GraphEdgeLabel, payload::RagRecord};
use anyhow::{Context, Result};
use petgraph::graph::Graph;
use serde_json::json;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
use tracing::info;

/// Write `AstNode`s as JSON Lines (`ast_nodes.jsonl`).
/// One node per line, serialized via `serde`.
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
/// Nodes are flattened into:
/// `{ id, name, type, file, start_line, end_line }`
///
/// Edges:
/// `{ src, dst, label }`
///
/// Indices are stable within this output (0..N-1) and consistent between nodes and edges.
pub fn write_graph_jsonl(
    nodes_path: &Path,
    edges_path: &Path,
    graph: &Graph<AstNode, GraphEdgeLabel>,
) -> Result<()> {
    // Build stable mapping: NodeIndex -> ordinal ID.
    let mut id_map = Vec::with_capacity(graph.node_count());
    for (i, nidx) in graph.node_indices().enumerate() {
        if nidx.index() >= id_map.len() {
            id_map.resize(nidx.index() + 1, usize::MAX);
        }
        id_map[nidx.index()] = i;
    }

    // Write nodes
    {
        let f =
            File::create(nodes_path).with_context(|| format!("create {}", nodes_path.display()))?;
        let mut w = BufWriter::new(f);
        for (i, nidx) in graph.node_indices().enumerate() {
            let n = &graph[nidx];
            let rec = json!({
                "id": i,
                "name": n.name,
                "type": ast_kind_key(&n.kind),
                "file": n.file,
                "start_line": n.span.start_line,
                "end_line": n.span.end_line,
            });
            serde_json::to_writer(&mut w, &rec)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        info!("jsonl: wrote graph nodes -> {}", nodes_path.display());
    }

    // Write edges
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

/// Write RAG records as JSON Lines (`rag_records.jsonl`).
/// These are the inputs for vectorization + Qdrant upsert later in your flow.
pub fn write_rag_records_jsonl(path: &Path, records: &[RagRecord]) -> Result<()> {
    let f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut w = BufWriter::new(f);
    for r in records {
        serde_json::to_writer(&mut w, r)?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    info!("jsonl: wrote RAG records -> {}", path.display());
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
