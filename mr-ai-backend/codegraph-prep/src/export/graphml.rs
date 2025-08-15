//! GraphML exporter for Gephi and similar tools.
//!
//! We flatten nodes to a small set of attributes and write directed edges with labels.
//! The format intentionally mirrors the JSONL export for easy cross-checking.

use crate::model::{ast::AstNode, graph::GraphEdgeLabel};
use anyhow::{Context, Result};
use petgraph::graph::Graph;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
use tracing::info;

/// Write GraphML to `path`.
pub fn write_graphml(path: &Path, graph: &Graph<AstNode, GraphEdgeLabel>) -> Result<()> {
    let f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut w = BufWriter::new(f);

    // Build stable IDs (n0..nN-1)
    let mut id_map: Vec<String> = Vec::with_capacity(graph.node_count());
    for (i, nidx) in graph.node_indices().enumerate() {
        if nidx.index() >= id_map.len() {
            id_map.resize(nidx.index() + 1, String::new());
        }
        id_map[nidx.index()] = format!("n{}", i);
    }

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
        let id = &id_map[nidx.index()];
        let n = &graph[nidx];
        writeln!(w, r#"<node id="{}">"#, id)?;
        writeln!(w, r#"  <data key="d0">{}</data>"#, xml_escape(&n.name))?;
        writeln!(
            w,
            r#"  <data key="d1">{}</data>"#,
            xml_escape(ast_kind_key(&n.kind))
        )?;
        writeln!(w, r#"  <data key="d2">{}</data>"#, xml_escape(&n.file))?;
        writeln!(w, r#"  <data key="d3">{}</data>"#, n.span.start_line)?;
        writeln!(w, r#"  <data key="d4">{}</data>"#, n.span.end_line)?;
        writeln!(w, r#"</node>"#)?;
    }

    // Edges
    for (i, eidx) in graph.edge_indices().enumerate() {
        let (src, dst) = graph.edge_endpoints(eidx).unwrap();
        let src_id = &id_map[src.index()];
        let dst_id = &id_map[dst.index()];
        let label = graph[eidx].to_string();
        writeln!(
            w,
            r#"<edge id="e{}" source="{}" target="{}">"#,
            i, src_id, dst_id
        )?;
        writeln!(w, r#"  <data key="e0">{}</data>"#, xml_escape(&label))?;
        writeln!(w, r#"</edge>"#)?;
    }

    writeln!(w, r#"</graph>"#)?;
    writeln!(w, r#"</graphml>"#)?;
    w.flush()?;
    info!("graphml: wrote -> {}", path.display());
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

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
