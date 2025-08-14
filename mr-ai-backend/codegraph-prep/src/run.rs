//! High-level orchestration for preparing AST/Graph/Qdrant context from a local codebase.
//!
//! This module contains the single public entry point `prepare_qdrant_context`.
//! It scans the given repository root, parses supported languages with Tree-sitter,
//! extracts AST entities and relations, builds a language-aware graph,
//! chunks large entities into AST-aware segments, and exports all artifacts
//! (JSONL/GraphML/summary + RAG payload) into `<root>/graphs_data/<timestamp>/`.

use crate::{
    config::model::GraphConfig,
    core::{chunking, fs_scan, parse, summary::PipelineSummary},
    export::save_all,
    graph::dispatcher,
    model::{ast::AstNode, payload::RagRecord},
};
use anyhow::Result;
use chrono::Utc;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Main pipeline: prepare AST, graph, and RAG payload from a local repository.
///
/// # Arguments
/// * `root` - Path to the root of the project/repository.
///
/// # Returns
/// [`save_all::PersistSummary`] - metadata with paths to generated artifacts.
///
/// # Steps:
/// 1. **Scan** filesystem, applying ignore/glob filters from config.
/// 2. **Parse** files with Tree-sitter for supported languages.
/// 3. **Extract** AST nodes and enrich with doc/signature/FQN/visibility/annotations.
/// 4. **Build graph**: language-aware relations (declares, imports, parts, etc.).
/// 5. **Chunking**: split large entities into AST-aware chunks with context.
/// 6. **Export** all artifacts into a timestamped folder under `<root>/graphs_data/`.
#[tracing::instrument(level = "info", skip_all, fields(root = %root))]
pub fn prepare_qdrant_context(root: &str) -> Result<save_all::PersistSummary> {
    let root_path = dunce::canonicalize(Path::new(root))?;

    // 1. Load config (could be from ENV, defaults, or file)
    // let config = GraphConfig::load_from_env_or_default(&root_path)?;
    let config = GraphConfig::default();
    info!("Configuration loaded");

    // 2. Scan filesystem: collect candidate files with language detection
    let scan_result = fs_scan::scan_repo(&root_path, &config)?;
    info!(files = scan_result.files.len(), "Scanned filesystem");

    // 3. Parse & extract AST nodes
    let mut ast_nodes: Vec<AstNode> = Vec::new();
    for file in &scan_result.files {
        if let Some(lang) = file.language {
            if let Err(err) = parse::parse_and_extract(file, lang, &mut ast_nodes, &config) {
                warn!(path = %file.path.display(), error = %err, "Failed to parse/extract");
            }
        }
    }
    info!(count = ast_nodes.len(), "Extracted AST nodes");

    // 4. Build language-aware graph
    let graph = dispatcher::build_language_aware_graph(&ast_nodes, &config)?;
    info!(
        nodes = graph.node_count(),
        edges = graph.edge_count(),
        "Built graph"
    );

    // 5. Chunk AST nodes for RAG
    let rag_records: Vec<RagRecord> = chunking::chunk_ast_nodes(&ast_nodes, &graph, &config)?;
    info!(count = rag_records.len(), "Generated RAG records");

    // 6. Prepare output folder
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let out_dir: PathBuf = root_path.join("graphs_data").join(timestamp);

    // 7. Save all artifacts (AST nodes, graph, RAG records, summary)
    let summary = save_all::persist_all(
        &out_dir,
        &ast_nodes,
        &graph,
        &rag_records,
        PipelineSummary::from_counts(&scan_result, &ast_nodes, &graph),
    )?;

    info!(out_dir = %out_dir.display(), "Artifacts saved");
    Ok(summary)
}
