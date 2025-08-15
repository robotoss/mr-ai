//! Persist all artifacts into the given output directory.
//!
//! Layout:
//!   out_dir/
//!     ast_nodes.jsonl
//!     graph_nodes.jsonl
//!     graph_edges.jsonl
//!     graph.graphml
//!     rag_records.jsonl
//!     summary.json
//!
//! `out_dir` is expected to be a timestamped folder (caller creates it).
//! This module ensures the directory exists and writes all files,
//! returning a `PersistSummary` containing paths and counts.

use crate::{
    core::summary::PipelineSummary,
    export::{graphml::write_graphml, jsonl, qdrant_prep},
    model::{ast::AstNode, graph::GraphEdgeLabel, payload::RagRecord},
};
use anyhow::{Context, Result};
use petgraph::graph::Graph;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::info;

/// File paths of the persisted artifacts (absolute).
#[derive(Debug, Clone, Serialize)]
pub struct PersistFiles {
    pub ast_nodes_jsonl: String,
    pub graph_nodes_jsonl: String,
    pub graph_edges_jsonl: String,
    pub graph_graphml: String,
    pub rag_records_jsonl: String,
    pub summary_json: String,
}

/// Top-level summary returned to the caller and also written to `summary.json`.
#[derive(Debug, Clone, Serialize)]
pub struct PersistSummary {
    pub out_dir: String,
    pub files: PersistFiles,
    pub counts_by_kind: BTreeMap<String, usize>,
    pub edge_labels: BTreeMap<String, usize>,
    pub summary: PipelineSummary,
}

/// Write all artifacts to `out_dir` and return `PersistSummary`.
///
/// Caller is responsible for choosing a timestamped directory; we just ensure it exists.
pub fn persist_all(
    out_dir: &Path,
    ast_nodes: &[AstNode],
    graph: &Graph<AstNode, GraphEdgeLabel>,
    rag_records: &[RagRecord],
    summary: PipelineSummary,
) -> Result<PersistSummary> {
    // Ensure directory exists.
    fs::create_dir_all(out_dir).with_context(|| format!("create_dir_all {}", out_dir.display()))?;
    info!("persist: dir prepared -> {}", out_dir.display());

    // Resolve paths
    let p_ast_nodes = out_dir.join("ast_nodes.jsonl");
    let p_gnodes = out_dir.join("graph_nodes.jsonl");
    let p_gedges = out_dir.join("graph_edges.jsonl");
    let p_graphml = out_dir.join("graph.graphml");
    let p_rag = out_dir.join("rag_records.jsonl");
    let p_summary = out_dir.join("summary.json");

    // Write artifact files
    jsonl::write_ast_nodes_jsonl(&p_ast_nodes, ast_nodes)?;
    jsonl::write_graph_jsonl(&p_gnodes, &p_gedges, graph)?;
    write_graphml(&p_graphml, graph)?;
    // Use either qdrant_prep or jsonl::write_rag_records_jsonl (identical content)
    qdrant_prep::write_qdrant_payload_jsonl(&p_rag, rag_records)?;

    // Aggregate counts
    let counts_by_kind = count_by_kind(ast_nodes);
    let edge_labels = count_edge_labels(graph);

    // Compose final summary
    let files = PersistFiles {
        ast_nodes_jsonl: p_ast_nodes.to_string_lossy().into_owned(),
        graph_nodes_jsonl: p_gnodes.to_string_lossy().into_owned(),
        graph_edges_jsonl: p_gedges.to_string_lossy().into_owned(),
        graph_graphml: p_graphml.to_string_lossy().into_owned(),
        rag_records_jsonl: p_rag.to_string_lossy().into_owned(),
        summary_json: p_summary.to_string_lossy().into_owned(),
    };
    let persist = PersistSummary {
        out_dir: out_dir.to_string_lossy().into_owned(),
        files: files.clone(),
        counts_by_kind,
        edge_labels,
        summary,
    };

    // Write summary.json (pretty)
    {
        let f = fs::File::create(&p_summary)
            .with_context(|| format!("create {}", p_summary.display()))?;
        let w = std::io::BufWriter::new(f);
        serde_json::to_writer_pretty(w, &persist)?;
    }

    info!("persist: all artifacts written");
    Ok(persist)
}

// --- helpers ---

fn count_by_kind(nodes: &[AstNode]) -> BTreeMap<String, usize> {
    use crate::model::ast::AstKind;
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    for n in nodes {
        let k = match n.kind {
            AstKind::File => "file",
            AstKind::Module => "module",
            AstKind::Package => "package",
            AstKind::Class => "class",
            AstKind::Mixin => "mixin",
            AstKind::Enum => "enum",
            AstKind::Extension => "extension",
            AstKind::ExtensionType => "extension_type",
            AstKind::Interface => "interface",
            AstKind::TypeAlias => "type_alias",
            AstKind::Trait => "trait",
            AstKind::Impl => "impl",
            AstKind::Function => "function",
            AstKind::Method => "method",
            AstKind::Field => "field",
            AstKind::Variable => "variable",
            AstKind::Import => "import",
            AstKind::Export => "export",
            AstKind::Part => "part",
            AstKind::PartOf => "part_of",
            AstKind::Macro => "macro",
        }
        .to_string();
        *m.entry(k).or_insert(0) += 1;
    }
    m
}

fn count_edge_labels(graph: &Graph<AstNode, GraphEdgeLabel>) -> BTreeMap<String, usize> {
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    for e in graph.edge_indices() {
        let key = graph[e].to_string();
        *m.entry(key).or_insert(0) += 1;
    }
    m
}
