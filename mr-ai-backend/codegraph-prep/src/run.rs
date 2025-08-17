use crate::{
    config::model::GraphConfig,
    core::{chunking, fs_scan, parse, summary::PipelineSummary},
    export::save_all,
    graph::dispatcher,
    model::{
        ast::AstNode,
        graph::GraphEdgeLabel,
        neighbors::{NeighborFillOptions, enrich_records_with_neighbors},
        payload::RagRecord,
    },
};
use anyhow::Result;
use chrono::Utc;
use petgraph::graph::Graph;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

pub fn prepare_qdrant_context(root: &str) -> Result<save_all::PersistSummary> {
    let root_path = dunce::canonicalize(Path::new(root))?;
    info!("pipeline: start -> {}", root_path.display());

    // 1) Config
    let config = GraphConfig::default();

    // 2) Scan
    let scan_result = fs_scan::scan_repo(&root_path, &config)?;
    info!("scan: {} files selected", scan_result.files.len());

    // 3) Parse & extract
    let mut ast_nodes: Vec<AstNode> = Vec::new();
    for file in &scan_result.files {
        if let Some(lang) = file.language {
            if let Err(err) = parse::parse_and_extract(file, lang, &mut ast_nodes, &config) {
                warn!("parse/extract failed: {} -> {}", file.path.display(), err);
            }
        }
    }
    info!("extract: {} AST nodes", ast_nodes.len());

    // 4) Build graph
    let graph: Graph<AstNode, GraphEdgeLabel> =
        dispatcher::build_language_aware_graph(&root_path, &ast_nodes, &config)?;
    info!(
        "graph: built (nodes={}, edges={})",
        graph.node_count(),
        graph.edge_count()
    );

    // 5a) Chunk -> RAG records
    let mut rag_records: Vec<RagRecord> =
        match chunking::chunk_ast_nodes(&ast_nodes, &graph, &config) {
            Ok(r) => {
                info!("chunking: {} records", r.len());
                r
            }
            Err(e) => {
                error!("chunking: failed: {}", e);
                Vec::new()
            }
        };

    // 5b) Enrich with neighbors from the graph (NEW STEP)
    // - attaches thin, ranked neighbor refs per record (by parent symbol)
    let neighbor_opts = NeighborFillOptions {
        max_neighbors_per_record: 24,
        include_outgoing: true,
        include_incoming: true,
        prefer_same_file: true,
        allowed_labels: None, // or Some(vec![...]) if need low
        declares_hops: 2,     // file -> class -> methods/vars
    };
    enrich_records_with_neighbors(&graph, &mut rag_records, &neighbor_opts);

    // 6) Output dir
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let out_dir: PathBuf = root_path.join("graphs_data").join(timestamp);

    // 7) Persist all
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
