use crate::{graphs::edge::GraphEdge, models::ast_node::ASTNode};
use anyhow::{Context, Result};
use petgraph::graph::Graph;
use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

/// Write GraphML suitable for Gephi.
pub fn write_graphml(path: &Path, graph: &Graph<ASTNode, GraphEdge>) -> Result<()> {
    let mut idx_map: HashMap<_, String> = HashMap::new();
    for (i, idx) in graph.node_indices().enumerate() {
        idx_map.insert(idx, format!("n{}", i));
    }

    let f = fs::File::create(path).with_context(|| format!("create {:?}", path))?;
    let mut w = BufWriter::new(f);

    // Header + keys
    writeln!(w, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        w,
        r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns"
    xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
    xsi:schemaLocation="http://graphml.graphdrawing.org/xmlns
     http://graphml.graphdrawing.org/xmlns/1.0/graphml.xsd">"#
    )?;

    // Node keys
    writeln!(
        w,
        r#"<key id="d0" for="node" attr.name="name" attr.type="string"/>"#
    )?;
    writeln!(
        w,
        r#"<key id="d1" for="node" attr.name="type" attr.type="string"/>"#
    )?;
    writeln!(
        w,
        r#"<key id="d2" for="node" attr.name="file" attr.type="string"/>"#
    )?;
    writeln!(
        w,
        r#"<key id="d3" for="node" attr.name="start_line" attr.type="int"/>"#
    )?;
    writeln!(
        w,
        r#"<key id="d4" for="node" attr.name="end_line" attr.type="int"/>"#
    )?;
    // Edge key
    writeln!(
        w,
        r#"<key id="e0" for="edge" attr.name="label" attr.type="string"/>"#
    )?;

    // Graph
    writeln!(w, r#"<graph edgedefault="directed">"#)?;

    // Nodes
    for nidx in graph.node_indices() {
        let id = &idx_map[&nidx];
        let n = &graph[nidx];
        writeln!(w, r#"<node id="{}">"#, id)?;
        writeln!(w, r#"  <data key="d0">{}</data>"#, xml_escape(&n.name))?;
        writeln!(w, r#"  <data key="d1">{}</data>"#, xml_escape(&n.node_type))?;
        writeln!(w, r#"  <data key="d2">{}</data>"#, xml_escape(&n.file))?;
        writeln!(w, r#"  <data key="d3">{}</data>"#, n.start_line)?;
        writeln!(w, r#"  <data key="d4">{}</data>"#, n.end_line)?;
        writeln!(w, r#"</node>"#)?;
    }

    // Edges
    for (i, eidx) in graph.edge_indices().enumerate() {
        let (src, dst) = graph.edge_endpoints(eidx).unwrap();
        let src_id = &idx_map[&src];
        let dst_id = &idx_map[&dst];
        let label = &graph[eidx].0;
        writeln!(
            w,
            r#"<edge id="e{}" source="{}" target="{}">"#,
            i, src_id, dst_id
        )?;
        writeln!(w, r#"  <data key="e0">{}</data>"#, xml_escape(label))?;
        writeln!(w, r#"</edge>"#)?;
    }

    // Close
    writeln!(w, r#"</graph>"#)?;
    writeln!(w, r#"</graphml>"#)?;
    w.flush()?;
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
