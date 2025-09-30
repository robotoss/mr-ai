//! Lightweight AST builder for Dart: imports, aliasing, and rough usage detection.

use crate::lsp::dart::parse::LspSymbolInfo;
use crate::lsp::dart::util::{DartImport, classify_origin_from_import, parse_imports_in_dart};
use crate::types::ImportUse;

/// Per-file AST surface we care about for RAG and code review signals.
#[derive(Debug, Clone)]
pub struct AstFile {
    pub file_key: String,
    pub imports: Vec<ImportUse>,
    /// A rough list of used imported identifiers found in the code.
    /// Examples: "GoRoute", "ValueNotifier", "Timer", "Ref", "Widget".
    pub uses: Vec<String>,
}

/// Build a lightweight AST for a file: imports + rough usage.
pub fn build_file_ast(file_key: String, code: &str, _syms: &[LspSymbolInfo]) -> AstFile {
    // Parse imports (uri + alias + show/hide → identifiers).
    let mut imports: Vec<ImportUse> = Vec::new();
    let parsed: Vec<DartImport> = parse_imports_in_dart(code);
    for imp in parsed {
        let label: String = imp.label();
        let origin = classify_origin_from_import(&imp.uri);
        // If explicit `show` identifiers exist, emit them; else emit alias or '*' wildcard.
        if !imp.show.is_empty() {
            for ident in imp.show {
                imports.push(ImportUse {
                    origin,
                    label: label.clone(),
                    identifier: ident,
                });
            }
        } else if let Some(alias) = imp.r#as {
            imports.push(ImportUse {
                origin,
                label: label,
                identifier: alias,
            });
        } else {
            imports.push(ImportUse {
                origin,
                label: label,
                identifier: "*".to_string(),
            });
        }
    }

    // Very lightweight usage detection:
    // - If `show` provided identifiers, count their occurrences.
    // - If alias exists, detect `<alias>.` member accesses.
    // This is heuristic by design and safe for enrichment/tagging.
    let mut uses: Vec<String> = Vec::new();
    for iu in &imports {
        if iu.identifier == "*" {
            continue;
        }
        let needle = if iu
            .identifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            iu.identifier.clone()
        } else {
            continue;
        };
        if code.contains(&needle) {
            uses.push(needle);
        }
        // alias.member pattern
        if !iu.identifier.is_empty()
            && iu
                .identifier
                .chars()
                .next()
                .map(|c| c.is_lowercase())
                .unwrap_or(false)
        {
            // probable alias like `router` or `go_router`
            let alias_pref = format!("{}.", iu.identifier);
            if code.contains(&alias_pref) {
                uses.push(iu.identifier.clone());
            }
        }
    }
    uses.sort();
    uses.dedup();

    AstFile {
        file_key,
        imports,
        uses,
    }
}
