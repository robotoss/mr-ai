//! High-level orchestration for preparing AST/Graph/Qdrant context from a local codebase.
//!
//! This module exposes a single public entry point: [`prepare_qdrant_context`].
//! It performs the following steps:
//! 1) Scan filesystem with ignore/glob filters;
//! 2) Parse supported languages via Tree-sitter and extract AST;
//! 3) Build a language-aware dependency graph;
//! 4) Chunk entities into RAG-ready records;
//! 5) Persist artifacts (JSONL/GraphML/summary + RAG payload) under `<root>/graphs_data/<timestamp>/`.

use crate::{
    config::model::GraphConfig,
    core::{chunking, fs_scan, parse, summary::PipelineSummary},
    export::save_all,
    graph::dispatcher,
    model::{ast::AstNode, graph::GraphEdgeLabel, payload::RagRecord},
};
use anyhow::Result;
use chrono::Utc;
use petgraph::graph::Graph;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

/// Main pipeline: prepare AST, graph, and RAG payload from a local repository.
///
/// # Arguments
/// * `root` - Path to the root of the project/repository.
///
/// # Returns
/// [`save_all::PersistSummary`] — metadata with paths to generated artifacts.
pub fn prepare_qdrant_context(root: &str) -> Result<save_all::PersistSummary> {
    // Canonicalize the input root to keep IDs and paths stable.
    let root_path = dunce::canonicalize(Path::new(root))?;
    info!("pipeline: start -> {}", root_path.display());

    // 1) Load configuration (defaults).
    let config = GraphConfig::default();

    // 2) Scan filesystem and collect files to parse.
    let scan_result = fs_scan::scan_repo(&root_path, &config)?;
    info!("scan: {} files selected", scan_result.files.len());

    // 3) Parse & extract AST.
    let mut ast_nodes: Vec<AstNode> = Vec::new();
    for file in &scan_result.files {
        if let Some(lang) = file.language {
            if let Err(err) = parse::parse_and_extract(file, lang, &mut ast_nodes, &config) {
                warn!("parse/extract failed: {} -> {}", file.path.display(), err);
            }
        }
    }
    info!("extract: {} AST nodes", ast_nodes.len());

    // 4) Build language-aware graph.
    let graph: Graph<AstNode, GraphEdgeLabel> =
        dispatcher::build_language_aware_graph(&root_path, &ast_nodes, &config)?;
    info!(
        "graph: built (nodes={}, edges={})",
        graph.node_count(),
        graph.edge_count()
    );

    // 5) Chunk AST nodes into RAG records.
    let rag_records: Vec<RagRecord> = match chunking::chunk_ast_nodes(&ast_nodes, &graph, &config) {
        Ok(r) => {
            info!("chunking: {} records", r.len());
            r
        }
        Err(e) => {
            // Do not fail the whole pipeline on chunking error; surface as a hard error if you prefer.
            error!("chunking: failed: {}", e);
            Vec::new()
        }
    };

    // 6) Prepare output directory.
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let out_dir: PathBuf = root_path.join("graphs_data").join(timestamp);

    // 7) Persist all artifacts to disk.
    let summary = save_all::persist_all(
        &out_dir,
        &ast_nodes,
        &graph,
        &rag_records,
        PipelineSummary::from_counts(&scan_result, &ast_nodes, &graph, root),
    )?;

    info!("persist: artifacts saved to {}", out_dir.display());
    info!("pipeline: done");
    Ok(summary)
}
