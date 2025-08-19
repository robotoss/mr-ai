//! Regex fallbacks for Dart extraction.
//!
//! Activated only when AST likely missed categories (e.g., zero directives or zero class-likes).
//! We append nodes if they are not already present (dedup by (kind,name,span,file)).

use crate::model::{
    ast::{AstKind, AstNode},
    span::Span,
};
use regex::Regex;
use std::path::Path;

pub fn maybe_apply_regex_fallbacks(code: &str, path: &Path, out: &mut Vec<AstNode>) {
    let file = path.to_string_lossy().to_string();

    let has_directives = out.iter().any(|n| {
        matches!(
            n.kind,
            AstKind::Import | AstKind::Export | AstKind::Part | AstKind::PartOf
        ) && n.file == file
    });
    let has_classlike = out.iter().any(|n| {
        matches!(
            n.kind,
            AstKind::Class
                | AstKind::Mixin
                | AstKind::Enum
                | AstKind::Extension
                | AstKind::ExtensionType
        ) && n.file == file
    });

    if !has_directives {
        scan_directives_by_regex(code, path, out);
    }
    if !has_classlike {
        scan_classlikes_by_regex(code, path, out);
    }
}

fn scan_directives_by_regex(code: &str, path: &Path, out: &mut Vec<AstNode>) {
    let re_ie =
        Regex::new(r#"(?m)^\s*(import|export|part)\s+(['"][^'"]+['"])(?:\s+as\s+([A-Za-z_]\w*))?"#)
            .unwrap();
    for cap in re_ie.captures_iter(code) {
        let kind = cap.get(1).unwrap().as_str();
        let uriq = cap.get(2).unwrap().as_str();
        let alias = cap.get(3).map(|m| m.as_str().to_string());
        let uri = strip_quotes(uriq);
        let (astk, name) = match kind {
            "import" => (AstKind::Import, uri.clone()),
            "export" => (AstKind::Export, uri.clone()),
            _ => (AstKind::Part, uri.clone()),
        };
        let line = line_of(code, cap.get(0).unwrap().start());
        push(
            path,
            out,
            astk,
            &name,
            Span::new(line, line, 0, 0),
            alias,
            try_resolve(path, &uri),
        );
    }

    let re_part_of =
        Regex::new(r#"(?m)^\s*part\s+of\s+((?:['"][^'"]+['"])|(?:[A-Za-z_]\w*))"#).unwrap();
    for cap in re_part_of.captures_iter(code) {
        let name = strip_quotes(cap.get(1).unwrap().as_str());
        let line = line_of(code, cap.get(0).unwrap().start());
        push(
            path,
            out,
            AstKind::PartOf,
            &name,
            Span::new(line, line, 0, 0),
            None,
            None,
        );
    }
}

fn scan_classlikes_by_regex(code: &str, path: &Path, out: &mut Vec<AstNode>) {
    let patterns = [
        (
            AstKind::Class,
            r#"(?m)^\s*(?:(?:abstract|base|interface|final|sealed)\s+)*class\s+([A-Za-z_]\w*)"#,
        ),
        (
            AstKind::Mixin,
            r#"(?m)^\s*(?:base\s+)?mixin\s+([A-Za-z_]\w*)"#,
        ),
        (
            AstKind::Class,
            r#"(?m)^\s*(?:(?:abstract|base|interface|final|sealed)\s+)*mixin\s+class\s+([A-Za-z_]\w*)"#,
        ),
        (AstKind::Enum, r#"(?m)^\s*enum\s+([A-Za-z_]\w*)"#),
        (
            AstKind::ExtensionType,
            r#"(?m)^\s*extension\s+type\s+([A-Za-z_]\w*)\s*\("#,
        ),
    ];
    for (k, pat) in patterns {
        let re = Regex::new(pat).unwrap();
        for cap in re.captures_iter(code) {
            let name = cap.get(1).unwrap().as_str().to_string();
            let line = line_of(code, cap.get(0).unwrap().start());
            push(
                path,
                out,
                k.clone(),
                &name,
                Span::new(line, line, 0, 0),
                None,
                None,
            );
        }
    }

    // extension (named and anonymous)
    let re_named = Regex::new(r#"(?m)^\s*extension\s+([A-Za-z_]\w*)\s+on\s+"#).unwrap();
    for cap in re_named.captures_iter(code) {
        let name = cap.get(1).unwrap().as_str().to_string();
        let line = line_of(code, cap.get(0).unwrap().start());
        push(
            path,
            out,
            AstKind::Extension,
            &name,
            Span::new(line, line, 0, 0),
            None,
            None,
        );
    }
    let re_anon = Regex::new(r#"(?m)^\s*extension\s+on\s+"#).unwrap();
    for cap in re_anon.captures_iter(code) {
        let line = line_of(code, cap.get(0).unwrap().start());
        push(
            path,
            out,
            AstKind::Extension,
            "extension",
            Span::new(line, line, 0, 0),
            None,
            None,
        );
    }
}

// --- helpers ---

fn push(
    path: &Path,
    out: &mut Vec<AstNode>,
    kind: AstKind,
    name: &str,
    span: Span,
    import_alias: Option<String>,
    resolved: Option<std::path::PathBuf>,
) {
    let file = path.to_string_lossy().to_string();
    let id = crate::core::ids::symbol_id(
        crate::model::language::LanguageKind::Dart,
        name,
        &span,
        &file,
        &kind,
    );

    // Try to extract a snippet of code from the source file using span boundaries.
    // If the span is invalid, fall back to None.
    let snippet = std::fs::read_to_string(&file).ok().and_then(|code| {
        let start = span.start_byte.min(code.len());
        let end = span.end_byte.min(code.len());
        if start < end {
            Some(code[start..end].trim().to_string())
        } else {
            None
        }
    });

    out.push(AstNode {
        symbol_id: id,
        name: name.to_string(),
        kind,
        language: crate::model::language::LanguageKind::Dart,
        file,
        span,
        owner_path: Vec::new(),
        fqn: name.to_string(),
        visibility: None,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        import_alias,
        resolved_target: resolved.map(|p| p.to_string_lossy().to_string()),
        is_generated: false,
        snippet, // new field added in AstNode
    });
}

fn try_resolve(src: &Path, spec: &str) -> Option<std::path::PathBuf> {
    if spec.starts_with("dart:") || spec.starts_with("package:") {
        return None;
    }
    crate::languages::dart::uri::resolve_relative(src, spec)
}

fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        t[1..t.len().saturating_sub(1)].to_string()
    } else {
        t.to_string()
    }
}

fn line_of(code: &str, byte_idx: usize) -> usize {
    code[..byte_idx].bytes().filter(|&b| b == b'\n').count() + 1
}
