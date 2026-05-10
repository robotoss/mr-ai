//! Dart implementation of `LanguageAnalyzer`.
//!
//! S3 strategy: piggy-back on the existing tree-sitter + LSP-enriched
//! `CodeChunk` shape, which already carries `imports`, `graph.calls_out`,
//! `graph.uses_types`, `graph.defines_types` plus the LSP-derived
//! `lsp.imports_used`. We project those into the language-agnostic
//! `NodeIntent` / `EdgeIntent` model.
//!
//! Coverage today:
//! - **Imports** — file → import label, deduped per file.
//! - **Defines** — file → symbol; class/extension/mixin → method/field.
//! - **Calls** — chunk fqn → callee fqn (string-based).
//! - **Inherits** — class → super symbol parsed from `signature` /
//!   `extras["dart.extends"]` when present, else `extras["dart.with"]` /
//!   `extras["dart.implements"]`.
//! - **TypeUses** — chunk fqn → type symbol used.
//! - **AsyncBoundary** — chunk fqn → marker node `async:<callee>` whenever
//!   the LSP enrichment marks the symbol as async. Cheap proxy until the
//!   sidecar lands.
//!
//! Reserved for the upcoming Dart Analyzer sidecar:
//! - `DataFlow` — inter-procedural data-flow edges.
//! - `ControlFlow` — basic-block edges within a body.
//! - `PackageDep` — pubspec-driven package graph (handled in S3-D when the
//!   sync_git path also reads `pubspec.yaml`).

use std::collections::HashSet;

use domain::{EdgeKind, NodeKind, ProviderKind};
use serde_json::Value as JsonValue;
use tracing::{debug, trace};

use crate::analyzer::intent::{AnalysisOutcome, Coverage, EdgeIntent, NodeIntent};
use crate::analyzer::LanguageAnalyzer;
use crate::types::{CodeChunk, LanguageKind, SymbolKind};

#[derive(Debug, Default, Clone)]
pub struct DartAnalyzer;

impl DartAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl LanguageAnalyzer for DartAnalyzer {
    fn name(&self) -> &'static str {
        "dart"
    }

    fn supported_languages(&self) -> &'static [&'static str] {
        &["dart"]
    }

    fn provider_hint(&self) -> Option<ProviderKind> {
        None
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> AnalysisOutcome {
        let mut outcome = AnalysisOutcome::default();
        let mut seen_files: HashSet<String> = HashSet::new();
        let mut seen_imports: HashSet<(String, String)> = HashSet::new();

        for chunk in chunks {
            if !matches!(chunk.language, LanguageKind::Dart) {
                continue;
            }

            // 1. Make sure the file node exists once per file.
            if seen_files.insert(chunk.file.clone()) {
                outcome
                    .nodes
                    .push(NodeIntent::file_node(&chunk.file, "dart"));
            }

            // 2. Symbol node for this chunk.
            outcome.nodes.push(NodeIntent {
                fqn: chunk.symbol_path.clone(),
                kind: map_symbol_kind(&chunk.kind),
                file: chunk.file.clone(),
                symbol: chunk.symbol.clone(),
                language: "dart".into(),
                content_sha256: Some(chunk.content_sha256.clone()),
                span_start: Some(chunk.span.start_byte as u32),
                span_end: Some(chunk.span.end_byte as u32),
            });

            // 3. Defines: file → symbol (and parent → symbol when known).
            push_edge(
                &mut outcome,
                EdgeIntent {
                    from_fqn: chunk.file.clone(),
                    to_fqn: chunk.symbol_path.clone(),
                    edge_type: EdgeKind::Defines,
                    weight: 1.0,
                    meta: None,
                },
            );
            if let Some(parent) = parent_fqn(&chunk.symbol_path) {
                if parent != chunk.file {
                    push_edge(
                        &mut outcome,
                        EdgeIntent {
                            from_fqn: parent,
                            to_fqn: chunk.symbol_path.clone(),
                            edge_type: EdgeKind::Defines,
                            weight: 1.0,
                            meta: None,
                        },
                    );
                }
            }

            // 4. Imports: file → import label (deduped per file).
            for imp in dedup(&chunk.imports) {
                if seen_imports.insert((chunk.file.clone(), imp.clone())) {
                    outcome.nodes.push(NodeIntent {
                        fqn: format!("import:{imp}"),
                        kind: NodeKind::Module,
                        file: chunk.file.clone(),
                        symbol: imp.clone(),
                        language: "dart".into(),
                        content_sha256: None,
                        span_start: None,
                        span_end: None,
                    });
                    push_edge(
                        &mut outcome,
                        EdgeIntent {
                            from_fqn: chunk.file.clone(),
                            to_fqn: format!("import:{imp}"),
                            edge_type: EdgeKind::Imports,
                            weight: 1.0,
                            meta: None,
                        },
                    );
                }
            }

            // 5. Edges from the chunk's own `graph` payload.
            if let Some(graph) = &chunk.graph {
                for call in dedup(&graph.calls_out) {
                    push_edge(
                        &mut outcome,
                        EdgeIntent {
                            from_fqn: chunk.symbol_path.clone(),
                            to_fqn: call.clone(),
                            edge_type: EdgeKind::Calls,
                            weight: 1.0,
                            meta: None,
                        },
                    );
                }
                for typ in dedup(&graph.uses_types) {
                    push_edge(
                        &mut outcome,
                        EdgeIntent {
                            from_fqn: chunk.symbol_path.clone(),
                            to_fqn: typ.clone(),
                            edge_type: EdgeKind::TypeUses,
                            weight: 1.0,
                            meta: None,
                        },
                    );
                }
            }

            // 6. Inheritance — pulled from per-language extras when present.
            if let Some(extras) = &chunk.extras {
                push_inherits_edges(&mut outcome, &chunk.symbol_path, extras);
            }

            // 7. Async-boundary marker (cheap, until the sidecar lands).
            if let Some(lsp) = &chunk.lsp {
                if lsp.tags.contains("async") || lsp.tags.contains("future") {
                    let boundary = format!("async-boundary:{}", chunk.symbol_path);
                    outcome.nodes.push(NodeIntent {
                        fqn: boundary.clone(),
                        kind: NodeKind::Custom("async_marker".into()),
                        file: chunk.file.clone(),
                        symbol: format!("async:{}", chunk.symbol),
                        language: "dart".into(),
                        content_sha256: None,
                        span_start: None,
                        span_end: None,
                    });
                    push_edge(
                        &mut outcome,
                        EdgeIntent {
                            from_fqn: chunk.symbol_path.clone(),
                            to_fqn: boundary,
                            edge_type: EdgeKind::AsyncBoundary,
                            weight: 1.0,
                            meta: None,
                        },
                    );
                }
            }
        }

        // Dedup nodes by fqn (keeps the last write — they share kind/language).
        dedup_nodes_by_fqn(&mut outcome.nodes);

        debug!(
            target = "analyzer.dart",
            nodes = outcome.nodes.len(),
            edges = outcome.edges.len(),
            coverage = ?outcome.coverage,
            "DartAnalyzer finished"
        );
        trace!(target = "analyzer.dart", outcome = ?outcome);
        outcome
    }
}

fn map_symbol_kind(kind: &SymbolKind) -> NodeKind {
    match kind {
        SymbolKind::Module => NodeKind::Module,
        SymbolKind::Import => NodeKind::Module,
        SymbolKind::Class => NodeKind::Class,
        SymbolKind::Interface => NodeKind::Interface,
        SymbolKind::Enum => NodeKind::Enum,
        SymbolKind::Mixin => NodeKind::Mixin,
        SymbolKind::Extension => NodeKind::Extension,
        SymbolKind::Function => NodeKind::Function,
        SymbolKind::Method => NodeKind::Method,
        SymbolKind::Constructor => NodeKind::Constructor,
        SymbolKind::Field => NodeKind::Field,
        SymbolKind::Variable => NodeKind::Variable,
        SymbolKind::Typedef => NodeKind::Typedef,
        SymbolKind::Unknown => NodeKind::Custom("unknown".into()),
    }
}

fn parent_fqn(symbol_path: &str) -> Option<String> {
    let mut parts: Vec<&str> = symbol_path.split("::").collect();
    if parts.len() <= 1 {
        return None;
    }
    parts.pop();
    Some(parts.join("::"))
}

fn dedup(items: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for s in items {
        if seen.insert(s.clone()) {
            out.push(s.clone());
        }
    }
    out
}

fn dedup_nodes_by_fqn(nodes: &mut Vec<NodeIntent>) {
    let mut seen: HashSet<String> = HashSet::new();
    nodes.retain(|n| seen.insert(n.fqn.clone()));
}

fn push_edge(outcome: &mut AnalysisOutcome, edge: EdgeIntent) {
    outcome.coverage.record(&edge.edge_type);
    outcome.edges.push(edge);
}

fn push_inherits_edges(outcome: &mut AnalysisOutcome, from_fqn: &str, extras: &JsonValue) {
    let pull_array = |key: &str| -> Vec<String> {
        extras
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let pull_string = |key: &str| -> Option<String> {
        extras
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };

    if let Some(super_name) = pull_string("dart.extends") {
        push_edge(
            outcome,
            EdgeIntent {
                from_fqn: from_fqn.to_owned(),
                to_fqn: super_name,
                edge_type: EdgeKind::Inherits,
                weight: 1.0,
                meta: Some(serde_json::json!({"relation": "extends"})),
            },
        );
    }
    for mixin in pull_array("dart.with") {
        push_edge(
            outcome,
            EdgeIntent {
                from_fqn: from_fqn.to_owned(),
                to_fqn: mixin,
                edge_type: EdgeKind::Inherits,
                weight: 0.7,
                meta: Some(serde_json::json!({"relation": "with"})),
            },
        );
    }
    for iface in pull_array("dart.implements") {
        push_edge(
            outcome,
            EdgeIntent {
                from_fqn: from_fqn.to_owned(),
                to_fqn: iface,
                edge_type: EdgeKind::Inherits,
                weight: 0.7,
                meta: Some(serde_json::json!({"relation": "implements"})),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ChunkFeatures, CodeChunk, GraphEdges, LanguageKind, LspEnrichment, Span, SymbolKind,
    };
    use std::collections::BTreeSet;

    fn span() -> Span {
        Span {
            start_byte: 0,
            end_byte: 10,
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 10,
        }
    }

    fn base_chunk(file: &str, symbol: &str, kind: SymbolKind) -> CodeChunk {
        CodeChunk {
            id: format!("{file}#{symbol}"),
            language: LanguageKind::Dart,
            file: file.to_owned(),
            symbol: symbol.to_owned(),
            symbol_path: format!("{file}::{symbol}"),
            kind,
            span: span(),
            owner_path: vec![],
            doc: None,
            annotations: vec![],
            imports: vec![],
            signature: None,
            is_definition: true,
            is_generated: false,
            snippet: None,
            features: ChunkFeatures::default(),
            content_sha256: "deadbeef".into(),
            neighbors: None,
            identifiers: vec![],
            anchors: vec![],
            graph: None,
            hints: None,
            lsp: None,
            extras: None,
        }
    }

    #[test]
    fn ignores_non_dart_chunks() {
        let mut chunk = base_chunk("foo.rs", "Foo", SymbolKind::Class);
        chunk.language = LanguageKind::Rust;
        let outcome = DartAnalyzer::new().analyze_chunks(&[chunk]);
        assert!(outcome.nodes.is_empty());
        assert!(outcome.edges.is_empty());
    }

    #[test]
    fn emits_file_node_and_defines_edge_per_symbol() {
        let chunk = base_chunk("lib/main.dart", "MyClass", SymbolKind::Class);
        let outcome = DartAnalyzer::new().analyze_chunks(&[chunk]);
        // file + class
        assert_eq!(outcome.nodes.len(), 2);
        assert!(outcome
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::File && n.fqn == "lib/main.dart"));
        assert!(outcome.edges.iter().any(|e| matches!(
            (&e.edge_type, e.from_fqn.as_str(), e.to_fqn.as_str()),
            (EdgeKind::Defines, "lib/main.dart", "lib/main.dart::MyClass")
        )));
        assert_eq!(outcome.coverage.defines, 1);
    }

    #[test]
    fn nested_define_edge_for_method_under_class() {
        let mut method = base_chunk("lib/main.dart", "build", SymbolKind::Method);
        method.symbol_path = "lib/main.dart::MyClass::build".into();
        let outcome = DartAnalyzer::new().analyze_chunks(&[method]);
        assert!(outcome.edges.iter().any(|e| matches!(
            (&e.edge_type, e.from_fqn.as_str(), e.to_fqn.as_str()),
            (
                EdgeKind::Defines,
                "lib/main.dart::MyClass",
                "lib/main.dart::MyClass::build"
            )
        )));
    }

    #[test]
    fn dedups_imports_per_file() {
        let mut a = base_chunk("lib/main.dart", "X", SymbolKind::Class);
        a.imports = vec!["package:flutter/material.dart".into()];
        let mut b = base_chunk("lib/main.dart", "Y", SymbolKind::Class);
        b.imports = vec![
            "package:flutter/material.dart".into(),
            "dart:async".into(),
        ];
        let outcome = DartAnalyzer::new().analyze_chunks(&[a, b]);
        let imports_edges: Vec<_> = outcome
            .edges
            .iter()
            .filter(|e| matches!(e.edge_type, EdgeKind::Imports))
            .collect();
        assert_eq!(imports_edges.len(), 2);
        assert_eq!(outcome.coverage.imports, 2);
    }

    #[test]
    fn picks_up_calls_and_type_uses_from_chunk_graph() {
        let mut chunk = base_chunk("lib/main.dart", "build", SymbolKind::Method);
        chunk.graph = Some(GraphEdges {
            calls_out: vec!["other_file.dart::other_func".into()],
            uses_types: vec!["BuildContext".into()],
            imports_out: vec![],
            defines_types: vec![],
            facts: Default::default(),
        });
        let outcome = DartAnalyzer::new().analyze_chunks(&[chunk]);
        assert!(outcome
            .edges
            .iter()
            .any(|e| matches!(e.edge_type, EdgeKind::Calls) && e.to_fqn == "other_file.dart::other_func"));
        assert!(outcome
            .edges
            .iter()
            .any(|e| matches!(e.edge_type, EdgeKind::TypeUses) && e.to_fqn == "BuildContext"));
    }

    #[test]
    fn picks_up_inheritance_from_extras() {
        let mut chunk = base_chunk("lib/main.dart", "MyApp", SymbolKind::Class);
        chunk.extras = Some(serde_json::json!({
            "dart.extends": "StatelessWidget",
            "dart.with": ["WidgetsBindingObserver"],
            "dart.implements": ["AppLifecycleListener"],
        }));
        let outcome = DartAnalyzer::new().analyze_chunks(&[chunk]);
        let inh_count = outcome
            .edges
            .iter()
            .filter(|e| matches!(e.edge_type, EdgeKind::Inherits))
            .count();
        assert_eq!(inh_count, 3);
    }

    #[test]
    fn emits_async_boundary_when_lsp_marks_async() {
        let mut chunk = base_chunk("lib/main.dart", "fetch", SymbolKind::Function);
        let mut lsp = LspEnrichment::default();
        let mut tags = BTreeSet::new();
        tags.insert("async".into());
        lsp.tags = tags;
        chunk.lsp = Some(lsp);
        let outcome = DartAnalyzer::new().analyze_chunks(&[chunk]);
        assert!(outcome
            .edges
            .iter()
            .any(|e| matches!(e.edge_type, EdgeKind::AsyncBoundary)));
        assert_eq!(outcome.coverage.async_boundary, 1);
    }
}
