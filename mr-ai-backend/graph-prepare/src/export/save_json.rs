use crate::{graph::GraphEdge, models::ast_node::ASTNode};
use anyhow::{Context, Result};
use petgraph::graph::Graph;
use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

/// Write AST nodes as JSON Lines (one node per line).
pub fn write_nodes_jsonl(path: &Path, nodes: &[ASTNode]) -> Result<()> {
    let f = fs::File::create(path).with_context(|| format!("create {:?}", path))?;
    let mut w = BufWriter::new(f);
    for n in nodes {
        serde_json::to_writer(&mut w, n)?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    Ok(())
}

/// Write graph nodes/edges as JSONL with stable numeric indices.
pub fn write_graph_jsonl(
    path_nodes: &Path,
    path_edges: &Path,
    graph: &Graph<ASTNode, GraphEdge>,
) -> Result<()> {
    let mut idx_map: HashMap<_, usize> = HashMap::new();
    for (i, idx) in graph.node_indices().enumerate() {
        idx_map.insert(idx, i);
    }

    // nodes
    {
        let f = fs::File::create(path_nodes).with_context(|| format!("create {:?}", path_nodes))?;
        let mut w = BufWriter::new(f);
        for (i, nidx) in graph.node_indices().enumerate() {
            let n = &graph[nidx];
            let rec = serde_json::json!({
                "id": i,
                "name": n.name,
                "type": n.node_type,
                "file": n.file,
                "start_line": n.start_line,
                "end_line": n.end_line
            });
            serde_json::to_writer(&mut w, &rec)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
    }

    // edges
    {
        let f = fs::File::create(path_edges).with_context(|| format!("create {:?}", path_edges))?;
        let mut w = BufWriter::new(f);
        for eidx in graph.edge_indices() {
            let (src, dst) = graph.edge_endpoints(eidx).unwrap();
            let src_id = idx_map[&src];
            let dst_id = idx_map[&dst];
            let label = &graph[eidx].0;
            let rec = serde_json::json!({
                "src": src_id,
                "dst": dst_id,
                "label": label
            });
            serde_json::to_writer(&mut w, &rec)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
    }

    Ok(())
}
