//! Graph dispatcher: bucketize nodes by language, call per-language linkers,
//! and merge results into a single graph.

use crate::{
    config::model::GraphConfig,
    graph::{dart_linker, generic, merge, py_linker, rs_linker, ts_linker},
    model::{ast::AstNode, graph::GraphEdgeLabel, language::LanguageKind},
};
use anyhow::Result;
use petgraph::graph::Graph;
use std::path::Path;
use tracing::info;

/// Build a language-aware graph:
/// - Dart → `dart_linker` (file-level imports/exports/part + declares, imports_via_export);
/// - TypeScript → `ts_linker` (imports/exports, fallback path mapping);
/// - Python → `py_linker` (absolute/relative imports, packages);
/// - Rust → `rs_linker` (mod/use, impl Trait for Type);
/// - Others / fallback → `generic` (same_file + simple imports).
///
/// `root` is used for best-effort relative resolution where possible.
pub fn build_language_aware_graph(
    root: &Path,
    nodes: &[AstNode],
    cfg: &GraphConfig,
) -> Result<Graph<AstNode, GraphEdgeLabel>> {
    info!("graph: dispatcher start, nodes={}", nodes.len());

    // Bucketize by language
    let mut buckets = Buckets::default();
    for n in nodes {
        match n.language {
            LanguageKind::Dart => buckets.dart.push(n.clone()),
            LanguageKind::TypeScript => buckets.ts.push(n.clone()),
            LanguageKind::JavaScript => buckets.js.push(n.clone()),
            LanguageKind::Python => buckets.py.push(n.clone()),
            LanguageKind::Rust => buckets.rs.push(n.clone()),
        }
    }

    // Build per-language subgraphs
    let mut subgraphs: Vec<Graph<AstNode, GraphEdgeLabel>> = Vec::new();

    if !buckets.dart.is_empty() {
        info!("graph: dart linker, {} nodes", buckets.dart.len());
        subgraphs.push(dart_linker::build(root, &buckets.dart, cfg)?);
    }

    // TS first; JS can fall back to generic if needed
    if !buckets.ts.is_empty() {
        info!("graph: ts linker, {} nodes", buckets.ts.len());
        subgraphs.push(ts_linker::build(root, &buckets.ts, cfg)?);
    }

    if !buckets.py.is_empty() {
        info!("graph: py linker, {} nodes", buckets.py.len());
        subgraphs.push(py_linker::build(root, &buckets.py, cfg)?);
    }

    if !buckets.rs.is_empty() {
        info!("graph: rs linker, {} nodes", buckets.rs.len());
        subgraphs.push(rs_linker::build(root, &buckets.rs, cfg)?);
    }

    // JS and anything else → generic
    let mut leftovers: Vec<AstNode> = Vec::new();
    leftovers.extend(buckets.js);
    // keep true leftovers: nodes with unknown/non-handled kinds could be added too
    // but our AST always has language set, so we only route JS here.
    if !leftovers.is_empty() {
        info!("graph: generic linker, {} nodes", leftovers.len());
        subgraphs.push(generic::build(root, &leftovers, cfg)?);
    }

    // Merge subgraphs
    let merged = merge::merge_graphs(subgraphs);

    info!(
        "graph: dispatcher done, nodes={}, edges={}",
        merged.node_count(),
        merged.edge_count()
    );
    Ok(merged)
}

#[derive(Default)]
struct Buckets {
    dart: Vec<AstNode>,
    ts: Vec<AstNode>,
    js: Vec<AstNode>,
    py: Vec<AstNode>,
    rs: Vec<AstNode>,
}
