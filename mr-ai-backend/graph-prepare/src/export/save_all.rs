use crate::export::{write_graph_jsonl, write_graphml, write_nodes_jsonl};
use crate::graphs::edge::GraphEdge;
use crate::models::ast_node::ASTNode;
use anyhow::{Context, Result};
use chrono::Utc;
use petgraph::graph::Graph;
use serde::Serialize;
use std::io::Write;
use std::{
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Clone)]
pub struct PersistSummary {
    pub root: String,
    pub out_dir: String,
    pub timestamp: String,
    pub ast_nodes: usize,
    pub graph_nodes: usize,
    pub graph_edges: usize,
    pub files: PersistFiles,
    pub timings_ms: TimingsMs,
    pub counts_by_type: std::collections::HashMap<String, usize>,
    pub edge_labels: std::collections::HashMap<String, usize>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PersistFiles {
    pub ast_nodes_jsonl: String,
    pub graph_nodes_jsonl: String,
    pub graph_edges_jsonl: String,
    pub graph_graphml: String,
    pub summary_json: String,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct TimingsMs {
    pub ast_collect: u128,
    pub graph_build: u128,
    pub total: u128,
}

/// Save all artifacts under `<root>/graphs_data/<timestamp>/`.
pub fn save_all(
    root: &str,
    nodes: &[ASTNode],
    graph: &Graph<ASTNode, GraphEdge>,
    timings: TimingsMs,
) -> Result<PersistSummary> {
    let target_dir = make_output_dir(root)?;
    let p_nodes = target_dir.join("ast_nodes.jsonl");
    let p_gnodes = target_dir.join("graph_nodes.jsonl");
    let p_gedges = target_dir.join("graph_edges.jsonl");
    let p_graphml = target_dir.join("graph.graphml");
    let p_summary = target_dir.join("summary.json");

    write_nodes_jsonl(&p_nodes, nodes)?;
    write_graph_jsonl(&p_gnodes, &p_gedges, graph)?;
    write_graphml(&p_graphml, graph)?;

    let files = PersistFiles {
        ast_nodes_jsonl: p_nodes.to_string_lossy().into_owned(),
        graph_nodes_jsonl: p_gnodes.to_string_lossy().into_owned(),
        graph_edges_jsonl: p_gedges.to_string_lossy().into_owned(),
        graph_graphml: p_graphml.to_string_lossy().into_owned(),
        summary_json: p_summary.to_string_lossy().into_owned(),
    };

    // after computing graph, before writing summary:
    let mut counts_by_type = std::collections::HashMap::new();
    for n in nodes {
        *counts_by_type.entry(n.node_type.clone()).or_insert(0) += 1;
    }

    let mut edge_labels = std::collections::HashMap::new();
    for eidx in graph.edge_indices() {
        let lbl = graph[eidx].0.clone();
        *edge_labels.entry(lbl).or_insert(0) += 1;
    }

    let summary = PersistSummary {
        root: root.to_string(),
        out_dir: target_dir.to_string_lossy().into_owned(),
        timestamp: target_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string(),
        ast_nodes: nodes.len(),
        graph_nodes: graph.node_count(),
        graph_edges: graph.edge_count(),
        files: files.clone(),
        timings_ms: timings,
        counts_by_type,
        edge_labels,
    };

    // write summary.json
    {
        let f = fs::File::create(&p_summary).with_context(|| format!("create {:?}", p_summary))?;
        let mut w = BufWriter::new(f);
        serde_json::to_writer_pretty(&mut w, &summary)?;
        w.flush()?;
    }

    Ok(summary)
}

fn make_output_dir(root: &str) -> Result<PathBuf> {
    let ts = Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let base = Path::new(root).join("graphs_data").join(ts);
    fs::create_dir_all(&base).with_context(|| format!("create_dir_all {:?}", base))?;
    Ok(base)
}
