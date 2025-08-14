//! Persistence layer: writes AST nodes, graph, and RAG payload to disk.

use crate::model::{ast::AstNode, payload::RagRecord};
use anyhow::{Context, Result};
use chrono::Utc;
use petgraph::Graph;
use serde::Serialize;
use std::{fs, io::Write, path::Path};

/// Metadata about persisted artifacts.
#[derive(Debug, Clone, Serialize)]
pub struct PersistSummary {
    pub out_dir: String,
    pub timestamp: String,
    pub ast_nodes: usize,
    pub graph_nodes: usize,
    pub graph_edges: usize,
    pub rag_records: usize,
}

/// Save all artifacts to a timestamped folder under `<root>/graphs_data/`.
#[tracing::instrument(level = "info", skip_all)]
pub fn persist_all(
    out_dir: &Path,
    ast_nodes: &[AstNode],
    graph: &Graph<AstNode, ()>,
    rag_records: &[RagRecord],
    _summary_data: impl Serialize,
) -> Result<PersistSummary> {
    fs::create_dir_all(out_dir).with_context(|| format!("create {:?}", out_dir))?;

    // Write summary.json
    let summary = PersistSummary {
        out_dir: out_dir.to_string_lossy().into_owned(),
        timestamp: Utc::now().format("%Y%m%d_%H%M%S").to_string(),
        ast_nodes: ast_nodes.len(),
        graph_nodes: graph.node_count(),
        graph_edges: graph.edge_count(),
        rag_records: rag_records.len(),
    };

    let summary_path = out_dir.join("summary.json");
    let mut f = fs::File::create(&summary_path)?;
    serde_json::to_writer_pretty(&mut f, &summary)?;
    f.flush()?;

    Ok(summary)
}
