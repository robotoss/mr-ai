//! High-level API: parse monorepo, build a language-aware graph, and persist artifacts.
//! This API returns nothing on success; all outputs are saved under
//! `root/graphs_data/<timestamp>/` (JSONL + GraphML + summary.json).

use crate::ast::parse_monorepo;
use crate::export::{TimingsMs, save_all};
use crate::graphs::build_graph_language_aware;
use anyhow::Result;
use std::time::Instant;

/// Parse, build, and persist. No values are returned to the caller.
pub fn parse_and_save_language_aware(root: &str) -> Result<()> {
    let t0 = Instant::now();

    // 1) Collect AST facts from all supported languages
    let t_ast = Instant::now();
    let nodes = parse_monorepo(root)?;
    let ast_ms = t_ast.elapsed().as_millis();

    // 2) Build a language-aware dependency graph (e.g., Dart gets proper file-level linking)
    let t_graph = Instant::now();
    let graph = build_graph_language_aware(root, &nodes)?;
    let graph_ms = t_graph.elapsed().as_millis();

    // 3) Persist artifacts under `root/graphs_data/<timestamp>/`
    //    (summary.json includes counts, timings, and file paths)
    let _ = save_all(
        root,
        &nodes,
        &graph,
        TimingsMs {
            ast_collect: ast_ms,
            graph_build: graph_ms,
            total: t0.elapsed().as_millis(),
        },
    )?;

    Ok(())
}
