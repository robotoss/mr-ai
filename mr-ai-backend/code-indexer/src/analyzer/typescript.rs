//! `TypescriptAnalyzer` — turns TypeScript `CodeChunk`s into graph
//! nodes / edges.
//!
//! S4B coverage:
//! - **Imports** — file → each `import 'x'` / `import ... from 'x'` /
//!   `export * from 'x'` source.
//! - **Defines** — file → each top-level declaration; parent
//!   (class/interface/namespace) → its method/field/property.
//! - **Calls** — chunk fqn → callee identifier. Best-effort regex
//!   over signature/snippet text, keyword-filtered. Replaced by the
//!   S4C `ts-morph`-based sidecar with a real call graph.
//! - **TypeUses** — capitalised identifiers in a chunk's signature.
//! - **Inherits** — `class A extends B implements I, J` and
//!   `interface I extends K` lift their `extends` / `implements`
//!   targets into Inherits edges.
//! - **AsyncBoundary** — signatures containing `async ` lift an
//!   `async:<name>` marker edge.
//!
//! Pure: never reads disk or talks to the network.

use std::collections::HashSet;

use domain::{EdgeKind, NodeKind, ProviderKind};
use regex::Regex;

use crate::analyzer::intent::{AnalysisOutcome, EdgeIntent, NodeIntent};
use crate::analyzer::LanguageAnalyzer;
use crate::types::{CodeChunk, LanguageKind, SymbolKind};

#[derive(Debug, Default, Clone)]
pub struct TypescriptAnalyzer;

impl TypescriptAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageAnalyzer for TypescriptAnalyzer {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn supported_languages(&self) -> &'static [&'static str] {
        &["typescript", "javascript"]
    }

    fn provider_hint(&self) -> Option<ProviderKind> {
        None
    }

    fn analyze_chunks(&self, chunks: &[CodeChunk]) -> AnalysisOutcome {
        let mut outcome = AnalysisOutcome::default();
        let mut seen_files: HashSet<String> = HashSet::new();
        let mut seen_imports: HashSet<(String, String)> = HashSet::new();

        let call_re = Regex::new(r"\b([A-Za-z_$][A-Za-z0-9_$]*)\s*\(").ok();
        let type_re = Regex::new(r"\b([A-Z][A-Za-z0-9_]*)\b").ok();
        let kw: HashSet<&str> = [
            "if", "else", "for", "while", "do", "return", "new", "throw", "try", "catch",
            "switch", "case", "break", "continue", "typeof", "instanceof", "in", "of", "void",
            "this", "super", "true", "false", "null", "undefined", "function", "class",
            "interface", "enum", "type", "namespace", "module", "as", "is", "from", "export",
            "import", "default", "async", "await", "yield", "Promise", "Array", "Map", "Set",
            "Record", "Partial", "Readonly", "Required", "Pick", "Omit", "Date", "String",
            "Number", "Boolean", "Object", "JSON", "console",
        ]
        .into_iter()
        .collect();

        for chunk in chunks {
            if !matches!(chunk.language, LanguageKind::Typescript) {
                continue;
            }

            if seen_files.insert(chunk.file.clone()) {
                outcome
                    .nodes
                    .push(NodeIntent::file_node(&chunk.file, "typescript"));
            }

            outcome.nodes.push(NodeIntent {
                fqn: chunk.symbol_path.clone(),
                kind: map_kind(&chunk.kind),
                file: chunk.file.clone(),
                symbol: chunk.symbol.clone(),
                language: "typescript".to_owned(),
                content_sha256: Some(chunk.content_sha256.clone()),
                span_start: u32::try_from(chunk.span.start_byte).ok(),
                span_end: u32::try_from(chunk.span.end_byte).ok(),
            });

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

            for imp in &chunk.imports {
                if seen_imports.insert((chunk.file.clone(), imp.clone())) {
                    push_edge(&mut outcome, &chunk.file, imp, EdgeKind::Imports);
                }
            }

            // Skip sub chunks for call/type-use scanning so a long
            // function body sliced into several windows doesn't
            // double-count identifiers.
            if matches!(chunk.chunk_kind, Some(crate::types::ChunkKind::Sub)) {
                continue;
            }

            let text_ref = chunk
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
                if text.contains("async ") {
                    push_edge(
                        &mut outcome,
                        &chunk.symbol_path,
                        &format!("async:{}", chunk.symbol),
                        EdgeKind::AsyncBoundary,
                    );
                }
            }

            // Inherits: `class A extends B` / `class A implements I, J`
            // / `interface I extends K`. The signature is the first
            // declaration line so a single regex over it covers all
            // three.
            if matches!(chunk.kind, SymbolKind::Class | SymbolKind::Interface) {
                if let Some(sig) = chunk.signature.as_deref() {
                    for base in parse_inherits(sig) {
                        push_edge(
                            &mut outcome,
                            &chunk.symbol_path,
                            &base,
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

/// Extract `extends` / `implements` targets from a class/interface
/// signature head. Returns an empty Vec when neither clause is present.
fn parse_inherits(sig: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    if let Ok(re) = Regex::new(r"extends\s+([\w.<>,\s]+?)(?:\{|implements|$)") {
        if let Some(cap) = re.captures(sig) {
            if let Some(m) = cap.get(1) {
                for piece in m.as_str().split(',') {
                    let t = piece.trim().split('<').next().unwrap_or("").trim();
                    if !t.is_empty() {
                        out.push(t.to_string());
                    }
                }
            }
        }
    }
    if let Ok(re) = Regex::new(r"implements\s+([\w.<>,\s]+?)(?:\{|$)") {
        if let Some(cap) = re.captures(sig) {
            if let Some(m) = cap.get(1) {
                for piece in m.as_str().split(',') {
                    let t = piece.trim().split('<').next().unwrap_or("").trim();
                    if !t.is_empty() {
                        out.push(t.to_string());
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inherits_handles_extends_and_implements() {
        let mut got = parse_inherits("export class App extends Base implements Greeter, Logger {");
        got.sort();
        assert_eq!(got, vec!["Base".to_string(), "Greeter".to_string(), "Logger".to_string()]);
    }

    #[test]
    fn parse_inherits_handles_interface_extends() {
        let got = parse_inherits("interface Greeter extends Closable {");
        assert_eq!(got, vec!["Closable".to_string()]);
    }

    #[test]
    fn parse_inherits_returns_empty_when_no_clauses() {
        assert!(parse_inherits("class App { foo() {} }").is_empty());
    }
}
