//! Persist all pipeline artifacts into a given output directory.
//!
//! Layout of `out_dir/` (caller provides timestamped path):
//!   - `ast_nodes.jsonl`
//!   - `graph_nodes.jsonl`
//!   - `graph_edges.jsonl`
//!   - `graph.graphml`
//!   - `rag_records.jsonl`
//!   - `summary.json`
//!
//! This module ensures the directory exists, writes all files, and returns a
//! [`PersistSummary`] with resolved paths and statistics.

use crate::{
    core::{normalize::normalize_repo_rel_str, summary::PipelineSummary},
    export::{graphml::write_graphml, jsonl, qdrant_prep},
    model::{
        ast::{AstKind, AstNode},
        graph::GraphEdgeLabel,
        payload::RagRecord,
    },
};
use anyhow::{Context, Result};
use petgraph::graph::Graph;
use serde::Serialize;
use std::{collections::BTreeMap, fs, path::Path};
use tracing::info;

/// File paths of all persisted artifacts (absolute, host-specific).
#[derive(Debug, Clone, Serialize)]
pub struct PersistFiles {
    pub ast_nodes_jsonl: String,
    pub graph_nodes_jsonl: String,
    pub graph_edges_jsonl: String,
    pub graph_graphml: String,
    pub rag_records_jsonl: String,
    pub summary_json: String,
}

/// Top-level summary returned to caller and also written to `summary.json`.
#[derive(Debug, Clone, Serialize)]
pub struct PersistSummary {
    /// Output directory (absolute).
    pub out_dir: String,
    /// Locations of all generated files.
    pub files: PersistFiles,
    /// Counts of AST nodes by kind (e.g. class, function, import).
    pub counts_by_kind: BTreeMap<String, usize>,
    /// Counts of graph edges by label (e.g. imports, exports).
    pub edge_labels: BTreeMap<String, usize>,
    /// High-level pipeline summary (scan, parse, graph, chunk stats).
    pub summary: PipelineSummary,
}

/// Write all artifacts to `out_dir` and return a [`PersistSummary`].
///
/// Caller is responsible for choosing a unique/timestamped directory.
/// We normalize all source paths into repo-relative strings before writing
/// artifacts, to keep outputs stable and portable across environments.
///
/// # Example
/// ```no_run
/// use std::path::Path;
/// use codegraph_prep::export::save_all::persist_all;
/// use codegraph_prep::model::{ast::AstNode, graph::GraphEdgeLabel, payload::RagRecord};
/// use petgraph::graph::Graph;
///
/// let out = Path::new("graphs_data/20250101_120000");
/// let ast_nodes: Vec<AstNode> = vec![];
/// let graph: Graph<AstNode, GraphEdgeLabel> = Graph::new();
/// let rag_records: Vec<RagRecord> = vec![];
/// let summary = Default::default();
///
/// let result = persist_all(out, &ast_nodes, &graph, &rag_records, summary).unwrap();
/// println!("Artifacts written to {}", result.out_dir);
/// ```
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

    // Resolve file paths.
    let p_ast_nodes = out_dir.join("ast_nodes.jsonl");
    let p_gnodes = out_dir.join("graph_nodes.jsonl");
    let p_gedges = out_dir.join("graph_edges.jsonl");
    let p_graphml = out_dir.join("graph.graphml");
    let p_rag = out_dir.join("rag_records.jsonl");
    let p_summary = out_dir.join("summary.json");

    // Normalize AST nodes and RAG records to repo-relative paths.
    let root = Path::new(&summary.root_folder);
    let ast_nodes_norm: Vec<AstNode> = ast_nodes
        .iter()
        .map(|n| n.with_normalized_path(root))
        .collect();
    let rag_records_norm: Vec<RagRecord> = rag_records
        .iter()
        .map(|r| r.with_normalized_path(root))
        .collect();

    // Write artifact files.
    jsonl::write_ast_nodes_jsonl(&p_ast_nodes, &ast_nodes_norm)?;
    jsonl::write_graph_jsonl(&p_gnodes, &p_gedges, graph, root)?;
    write_graphml(&p_graphml, graph, root)?;
    qdrant_prep::write_qdrant_payload_jsonl(&p_rag, &rag_records_norm)?;

    // Aggregate counts.
    let counts_by_kind = count_by_kind(&ast_nodes_norm);
    let edge_labels = count_edge_labels(graph);

    // Compose final summary.
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

    // Write summary.json (pretty).
    let f =
        fs::File::create(&p_summary).with_context(|| format!("create {}", p_summary.display()))?;
    let w = std::io::BufWriter::new(f);
    serde_json::to_writer_pretty(w, &persist)?;

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

// --- extension helpers for normalization ---

trait NormalizePath {
    fn with_normalized_path(&self, root: &Path) -> Self;
}

impl NormalizePath for AstNode {
    fn with_normalized_path(&self, root: &Path) -> Self {
        let mut cloned = self.clone();

        // Always normalize `file`
        cloned.file = normalize_repo_rel_str(root, Path::new(&self.file));

        // For file nodes: also normalize `name` (so graph nodes/graphml get repo-relative ID)
        if cloned.kind == AstKind::File {
            cloned.name = cloned.file.clone();
        }

        // For import/export/part nodes: normalize resolved target if present
        if matches!(
            cloned.kind,
            AstKind::Import | AstKind::Export | AstKind::Part | AstKind::PartOf
        ) {
            if let Some(ref target) = cloned.resolved_target {
                cloned.resolved_target = Some(normalize_repo_rel_str(root, Path::new(target)));
            }
        }

        cloned
    }
}

impl NormalizePath for RagRecord {
    fn with_normalized_path(&self, root: &Path) -> Self {
        let mut cloned = self.clone();

        // Always normalize the file path
        cloned.path = normalize_repo_rel_str(root, Path::new(&self.path));

        // If this is a file-level record, normalize the name as well
        if cloned.kind.to_lowercase() == "file" {
            cloned.name = cloned.path.clone();
        }

        cloned
    }
}
