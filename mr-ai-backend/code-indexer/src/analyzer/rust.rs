//! `RustAnalyzer` — turns Rust `CodeChunk`s into graph nodes / edges.
//!
//! S4A coverage:
//! - **Imports** — one edge per unique `use` / `extern crate` path the
//!   extractor collected.
//! - **Defines** — file → each top-level symbol; parent (impl/trait/mod)
//!   → its inner symbol.
//! - **Calls** — chunk fqn → callee fqn. Best-effort: identifiers
//!   followed by `(` inside the chunk text. Filtered to avoid Rust
//!   keywords. The S4C `syn`-based sidecar replaces this with a real
//!   intra-procedural call graph.
//! - **Inherits** — `impl T for U` → both `T` and `U` get inherits
//!   edges from the impl block.
//! - **TypeUses** — capitalised identifiers in a chunk's signature.
//! - **AsyncBoundary** — function/method whose signature contains
//!   `async fn` lands an `async:<name>` marker edge so retrieval can
//!   surface async boundaries without re-parsing.
//!
//! Pure: never reads disk or talks to the network.

use std::collections::HashSet;

use domain::{EdgeKind, NodeKind, ProviderKind};
use regex::Regex;

use crate::analyzer::intent::{AnalysisOutcome, EdgeIntent, NodeIntent};
use crate::analyzer::LanguageAnalyzer;
use crate::types::{CodeChunk, LanguageKind, SymbolKind};

#[derive(Debug, Default, Clone)]
pub struct RustAnalyzer;

impl RustAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageAnalyzer for RustAnalyzer {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn supported_languages(&self) -> &'static [&'static str] {
        &["rust"]
    }

    fn provider_hint(&self) -> Option<ProviderKind> {
        None
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> AnalysisOutcome {
        let mut outcome = AnalysisOutcome::default();
        let mut seen_files: HashSet<String> = HashSet::new();
        let mut seen_imports: HashSet<(String, String)> = HashSet::new();

        let call_re = Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").ok();
        let type_re = Regex::new(r"\b([A-Z][A-Za-z0-9_]*)\b").ok();
        let kw: HashSet<&str> = [
            "if", "match", "for", "while", "loop", "return", "let", "fn", "as", "in", "ref",
            "move", "self", "Self", "super", "crate", "Box", "Vec", "String", "Option", "Result",
            "Some", "None", "Ok", "Err", "true", "false", "where", "impl", "pub", "mut", "const",
            "use", "mod", "struct", "enum", "trait", "type",
        ]
        .into_iter()
        .collect();

        for chunk in chunks {
            if !matches!(chunk.language, LanguageKind::Rust) {
                continue;
            }

            // File node (once per file).
            if seen_files.insert(chunk.file.clone()) {
                outcome.nodes.push(NodeIntent::file_node(&chunk.file, "rust"));
            }

            // Symbol node.
            outcome.nodes.push(NodeIntent {
                fqn: chunk.symbol_path.clone(),
                kind: map_kind(&chunk.kind),
                file: chunk.file.clone(),
                symbol: chunk.symbol.clone(),
                language: "rust".to_owned(),
                content_sha256: Some(chunk.content_sha256.clone()),
                span_start: u32::try_from(chunk.span.start_byte).ok(),
                span_end: u32::try_from(chunk.span.end_byte).ok(),
            });

            // Defines: file → symbol if top-level; otherwise immediate
            // parent → symbol.
            let parent_fqn = chunk
                .parent_symbol_id
                .clone()
                .unwrap_or_else(|| chunk.file.clone());
            push_edge(
                &mut outcome,
                &parent_fqn,
                &chunk.symbol_path,
                EdgeKind::Defines,
            );

            // Imports: file → each unique import path.
            for imp in &chunk.imports {
                if seen_imports.insert((chunk.file.clone(), imp.clone())) {
                    push_edge(&mut outcome, &chunk.file, imp, EdgeKind::Imports);
                }
            }

            // Calls + type-uses from the chunk's body text. Skip sub
            // chunks — their bodies are slices of the parent we already
            // scanned, double-counting helps nobody.
            if matches!(chunk.chunk_kind, Some(crate::types::ChunkKind::Sub)) {
                continue;
            }

            let text_ref: Option<&str> = chunk
                .signature
                .as_deref()
                .or(chunk.snippet.as_deref());
            if let Some(text) = text_ref {
                if let Some(re) = &call_re {
                    for cap in re.captures_iter(text) {
                        if let Some(m) = cap.get(1) {
                            let callee = m.as_str();
                            if kw.contains(callee) {
                                continue;
                            }
                            push_edge(
                                &mut outcome,
                                &chunk.symbol_path,
                                callee,
                                EdgeKind::Calls,
                            );
                        }
                    }
                }
                if let Some(re) = &type_re {
                    for cap in re.captures_iter(text) {
                        if let Some(m) = cap.get(1) {
                            let t = m.as_str();
                            if kw.contains(t) {
                                continue;
                            }
                            push_edge(
                                &mut outcome,
                                &chunk.symbol_path,
                                t,
                                EdgeKind::TypeUses,
                            );
                        }
                    }
                }

                // Async boundary marker.
                if text.contains("async fn") || text.contains("async ") {
                    push_edge(
                        &mut outcome,
                        &chunk.symbol_path,
                        &format!("async:{}", chunk.symbol),
                        EdgeKind::AsyncBoundary,
                    );
                }
            }

            // Inherits: for impl blocks `impl Trait for Type` the
            // signature carries both names.
            if matches!(chunk.kind, SymbolKind::Extension) {
                if let Some(sig) = chunk.signature.as_deref() {
                    if let Some((trait_name, type_name)) = parse_impl_for(sig) {
                        push_edge(
                            &mut outcome,
                            &chunk.symbol_path,
                            &type_name,
                            EdgeKind::Inherits,
                        );
                        push_edge(
                            &mut outcome,
                            &chunk.symbol_path,
                            &trait_name,
                            EdgeKind::Inherits,
                        );
                    }
                }
            }
        }

        outcome
    }
}

fn push_edge(outcome: &mut AnalysisOutcome, from: &str, to: &str, kind: EdgeKind) {
    outcome.coverage.record(&kind);
    outcome.edges.push(EdgeIntent {
        from_fqn: from.to_owned(),
        to_fqn: to.to_owned(),
        edge_type: kind,
        weight: 1.0,
        meta: None,
    });
}

fn map_kind(k: &SymbolKind) -> NodeKind {
    match k {
        SymbolKind::Module => NodeKind::Module,
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
        SymbolKind::Import => NodeKind::Custom("import".into()),
        SymbolKind::Unknown => NodeKind::Custom("unknown".into()),
    }
}

/// Parse `impl <Trait> for <Type> { ... }` returning `(Trait, Type)` if
/// the signature matches. Returns `None` for inherent impls or
/// signatures we can't pattern-match cheaply.
fn parse_impl_for(sig: &str) -> Option<(String, String)> {
    let re = Regex::new(r"impl(?:<[^>]+>)?\s+([\w:]+(?:<[^>]+>)?)\s+for\s+([\w:]+(?:<[^>]+>)?)").ok()?;
    let cap = re.captures(sig)?;
    let trait_name = cap.get(1)?.as_str().to_string();
    let type_name = cap.get(2)?.as_str().to_string();
    Some((trait_name, type_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_impl_for_extracts_trait_and_type() {
        let (tr, ty) = parse_impl_for("impl Display for AppState { ... }").unwrap();
        assert_eq!(tr, "Display");
        assert_eq!(ty, "AppState");
    }

    #[test]
    fn parse_impl_for_returns_none_on_inherent_impl() {
        assert!(parse_impl_for("impl AppState { fn new() {} }").is_none());
    }

    #[test]
    fn analyzer_emits_file_imports_and_defines_for_rust_chunks() {
        use crate::ast::rust::RustAst;
        use crate::ast::interface::AstProvider;
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let _ = tmp
            .write_all(
                br#"
use std::collections::HashMap;

pub struct App {
    pub name: String,
}

impl App {
    pub fn new() -> Self { Self { name: String::new() } }
}
"#,
            );
        let path = tmp.path().with_extension("rs");
        std::fs::copy(tmp.path(), &path).unwrap();
        let chunks = RustAst::parse_file(&path).expect("parse rust fixture");
        std::fs::remove_file(&path).ok();

        let outcome = RustAnalyzer::new().analyze_chunks(&chunks);
        let import_count = outcome
            .edges
            .iter()
            .filter(|e| matches!(e.edge_type, EdgeKind::Imports))
            .count();
        let defines = outcome
            .edges
            .iter()
            .filter(|e| matches!(e.edge_type, EdgeKind::Defines))
            .count();
        assert!(
            import_count >= 1,
            "expected at least one Imports edge, got {import_count}"
        );
        assert!(
            defines >= 2,
            "expected Defines edges (file→symbol + impl→method), got {defines}"
        );
        // The file node should exist exactly once.
        let file_nodes = outcome
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::File))
            .count();
        assert_eq!(file_nodes, 1);
    }
}
