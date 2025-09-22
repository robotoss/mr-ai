//! Dart parsing and chunk extraction using tree-sitter.

use crate::errors::{Error, Result};
use crate::types::{ChunkFeatures, CodeChunk, LanguageKind, Span, SymbolKind, clamp_snippet};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

/// Returns the Dart language for tree-sitter from the orchard grammar crate.
#[inline]
fn dart_language() -> Language {
    tree_sitter_dart_orchard::LANGUAGE.into()
}

pub struct DartAst;

impl DartAst {
    /// Parse source code into a tree-sitter Tree.
    fn parse(code: &str) -> Result<tree_sitter::Tree> {
        let mut parser = Parser::new();
        let lang = dart_language();
        parser
            .set_language(&lang)
            .map_err(|_| Error::TreeSitterLanguage)?;
        parser.parse(code, None).ok_or(Error::TreeSitterParse)
    }
}

impl crate::ast::interface::AstProvider for DartAst {
    fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        let code = fs::read_to_string(path)?;
        let tree = Self::parse(&code)?;
        let file = path.to_string_lossy().to_string();
        let is_generated = looks_generated(&file);
        let mut chunks = extract_chunks(&tree, &code, &file, is_generated)?;

        // Attach bounded snippet for retrieval/embedding.
        for c in &mut chunks {
            if c.snippet.is_none() {
                let s = &code[c.span.start_byte..c.span.end_byte];
                c.snippet = Some(clamp_snippet(s, 2400, 120));
            }
        }
        compute_neighbors_in_file(&mut chunks);
        Ok(chunks)
    }
}

fn extract_chunks(
    tree: &tree_sitter::Tree,
    code: &str,
    file: &str,
    is_generated: bool,
) -> Result<Vec<CodeChunk>> {
    let lang = dart_language();
    let root = tree.root_node();

    let q = r#"
      (import_or_export (import_specification (uri) @import.uri)) @import.node

      (class_declaration name: (identifier) @class.name) @class.node
      (mixin_declaration  name: (identifier) @mixin.name) @mixin.node
      (extension_declaration name: (identifier) @ext.name) @ext.node
      (enum_declaration name: (identifier) @enum.name) @enum.node

      (function_declaration name: (identifier) @tlfn.name) @tlfn.node
      (method_declaration name: (identifier) @method.name) @method.node
      (constructor_declaration name: (identifier)? @ctor.name) @ctor.node

      (field_declaration (variable_declaration_list) @field.vdl) @field.node
      (top_level_variable_declaration (variable_declaration_list) @tlvar.vdl) @tlvar.node

      ((variable_declaration_list (initialized_identifier_list (initialized_identifier name: (identifier) @v.name))))
      ((variable_declaration_list (initialized_identifier_list (identifier) @v.name)))
    "#;

    // Build query (takes &Language).
    let query = Query::new(&lang, q).map_err(|_| Error::TreeSitterParse)?;

    // -------- pass 1: collect imports (use explicit iterator .next()) --------
    let mut imports = Vec::<String>::new();
    {
        let mut qc1 = QueryCursor::new();
        let mut it = qc1.matches(&query, root, code.as_bytes());
        while let Some(m) = it.next() {
            for cap in m.captures {
                if query.capture_names()[cap.index as usize] == "import.uri" {
                    let raw = cap.node.utf8_text(code.as_bytes()).unwrap_or_default();
                    let uri = raw.trim().trim_matches('"').to_string();
                    if !uri.is_empty() {
                        imports.push(uri);
                    }
                }
            }
        }
        imports.sort();
        imports.dedup();
    }

    // Reusable helpers.
    let make_span = |n: Node| Span {
        start_byte: n.start_byte(),
        end_byte: n.end_byte(),
        start_row: n.start_position().row,
        start_col: n.start_position().column,
        end_row: n.end_position().row,
        end_col: n.end_position().column,
    };
    let sha = |bytes: &[u8]| -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    };
    let make_id = |symbol_path: &str, sp: &Span| -> String {
        let mut h = Sha256::new();
        h.update(file.as_bytes());
        h.update(symbol_path.as_bytes());
        h.update(sp.start_byte.to_le_bytes());
        h.update(sp.end_byte.to_le_bytes());
        format!("{:x}", h.finalize())
    };
    let owner_chain_for = |n: Node| -> Vec<String> {
        let mut chain = Vec::<String>::new();
        let mut cur = n;
        while let Some(p) = cur.parent() {
            match p.kind() {
                "class_declaration"
                | "mixin_declaration"
                | "extension_declaration"
                | "enum_declaration" => {
                    if let Some(nn) = p.child_by_field_name("name") {
                        if let Ok(t) = nn.utf8_text(code.as_bytes()) {
                            if is_ident_like(t) {
                                chain.push(t.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
            cur = p;
        }
        chain.reverse();
        chain
    };
    let leading_meta = |n: Node| -> (Option<String>, Vec<String>) {
        let mut doc_lines = Vec::<String>::new();
        let mut ann = Vec::<String>::new();
        let mut cur = n;
        while let Some(prev) = cur.prev_sibling() {
            match prev.kind() {
                "comment" | "documentation_comment" => {
                    let t = prev.utf8_text(code.as_bytes()).unwrap_or_default();
                    let tt = t.trim();
                    if tt.starts_with("///") || tt.starts_with("/**") {
                        doc_lines.push(tt.to_string());
                        cur = prev;
                        continue;
                    } else {
                        break;
                    }
                }
                "metadata" => {
                    let t = prev
                        .utf8_text(code.as_bytes())
                        .unwrap_or_default()
                        .replace('\n', " ");
                    if let Some(name) = t.trim().strip_prefix('@') {
                        let name = name.split('(').next().unwrap_or("").trim().to_string();
                        if !name.is_empty() {
                            ann.push(name);
                        }
                    }
                    cur = prev;
                    continue;
                }
                _ => break,
            }
        }
        doc_lines.reverse();
        (
            if doc_lines.is_empty() {
                None
            } else {
                Some(doc_lines.join("\n"))
            },
            ann,
        )
    };
    let signature_of = |n: Node| -> Option<String> {
        let text = n.utf8_text(code.as_bytes()).ok()?.trim();
        Some(first_line(text, 240))
    };

    // -------- pass 2: build chunks (QueryMatches is not IntoIterator; use explicit iterator .next()) --------
    let mut out = Vec::<CodeChunk>::new();
    let mut qc2 = QueryCursor::new();
    let mut it2 = qc2.matches(&query, root, code.as_bytes());
    while let Some(m) = it2.next() {
        let mut decl_node: Option<Node> = None;
        let mut name: Option<String> = None;
        let mut kind: Option<SymbolKind> = None;
        let mut vdl_node: Option<Node> = None;

        for cap in m.captures {
            let cname = query.capture_names()[cap.index as usize];
            match cname {
                "class.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Class);
                }
                "class.name" => {
                    name = Some(read_ident(code, cap.node));
                }

                "mixin.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Mixin);
                }
                "mixin.name" => {
                    name = Some(read_ident(code, cap.node));
                }

                "ext.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Extension);
                }
                "ext.name" => {
                    name = Some(read_ident(code, cap.node));
                }

                "enum.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Enum);
                }
                "enum.name" => {
                    name = Some(read_ident(code, cap.node));
                }

                "tlfn.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Function);
                }
                "tlfn.name" => {
                    name = Some(read_ident(code, cap.node));
                }

                "method.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Method);
                }
                "method.name" => {
                    name = Some(read_ident(code, cap.node));
                }

                "ctor.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Constructor);
                }
                "ctor.name" => {
                    name = match read_ident_opt(code, cap.node) {
                        Some(s) if !s.is_empty() => Some(s),
                        _ => Some("<constructor>".to_string()),
                    };
                }

                "field.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Field);
                }
                "field.vdl" => {
                    vdl_node = Some(cap.node);
                }

                "tlvar.node" => {
                    decl_node = Some(cap.node);
                    kind = Some(SymbolKind::Variable);
                }
                "tlvar.vdl" => {
                    vdl_node = Some(cap.node);
                }

                _ => {}
            }
        }

        if let (Some(node), Some(k)) = (decl_node, kind.clone()) {
            match k {
                SymbolKind::Field | SymbolKind::Variable => {}
                _ => {
                    let owner = owner_chain_for(node);
                    let symbol = name.unwrap_or_else(|| "<anonymous>".to_string());
                    let symbol_path = if owner.is_empty() {
                        format!("{file}::{symbol}")
                    } else {
                        format!("{}::{}", file, owner.join("::")) + &format!("::{symbol}")
                    };
                    let (doc, annotations) = leading_meta(node);
                    let signature = signature_of(node);
                    let span = make_span(node);
                    let text = &code[span.start_byte..span.end_byte];
                    let features = ChunkFeatures {
                        byte_len: span.end_byte.saturating_sub(span.start_byte),
                        line_count: span.end_row.saturating_sub(span.start_row) + 1,
                        has_doc: doc.is_some(),
                        has_annotations: !annotations.is_empty(),
                    };
                    out.push(CodeChunk {
                        id: make_id(&symbol_path, &span),
                        language: LanguageKind::Dart,
                        file: file.to_string(),
                        symbol,
                        symbol_path,
                        kind: k,
                        span,
                        owner_path: owner,
                        doc,
                        annotations,
                        imports: imports.clone(),
                        signature,
                        is_definition: true,
                        is_generated,
                        snippet: None,
                        features,
                        content_sha256: sha(text.as_bytes()),
                        neighbors: None,
                        lsp: None,
                    });
                }
            }
        }

        if let (Some(node), Some(k @ (SymbolKind::Field | SymbolKind::Variable))) = (vdl_node, kind)
        {
            let names = collect_names_in_vdl(node, code);
            let owner = owner_chain_for(node);
            for sym in names {
                let symbol_path = if owner.is_empty() {
                    format!("{file}::{sym}")
                } else {
                    format!("{}::{}", file, owner.join("::")) + &format!("::{sym}")
                };
                let (doc, annotations) = leading_meta(node);
                let signature = signature_of(node);
                let span = make_span(node);
                let text = &code[span.start_byte..span.end_byte];
                let features = ChunkFeatures {
                    byte_len: span.end_byte.saturating_sub(span.start_byte),
                    line_count: span.end_row.saturating_sub(span.start_row) + 1,
                    has_doc: doc.is_some(),
                    has_annotations: !annotations.is_empty(),
                };
                out.push(CodeChunk {
                    id: make_id(&symbol_path, &span),
                    language: LanguageKind::Dart,
                    file: file.to_string(),
                    symbol: sym,
                    symbol_path,
                    kind: k.clone(),
                    span,
                    owner_path: owner.clone(),
                    doc: doc.clone(),
                    annotations: annotations.clone(),
                    imports: imports.clone(),
                    signature: signature.clone(),
                    is_definition: true,
                    is_generated,
                    snippet: None,
                    features,
                    content_sha256: sha(text.as_bytes()),
                    neighbors: None,
                    lsp: None,
                });
            }
        }
    }

    dedup_chunks(&mut out);
    Ok(out)
}

fn compute_neighbors_in_file(chunks: &mut [CodeChunk]) {
    chunks.sort_by_key(|c| c.span.start_byte);
    for i in 0..chunks.len() {
        let prev = if i > 0 {
            Some(chunks[i - 1].id.clone())
        } else {
            None
        };
        let next = if i + 1 < chunks.len() {
            Some(chunks[i + 1].id.clone())
        } else {
            None
        };
        let entry = chunks[i].neighbors.get_or_insert_with(Default::default);
        entry.prev_id = prev;
        entry.next_id = next;
    }

    use std::collections::HashMap;
    let mut by_path = HashMap::<String, usize>::new();
    for (i, c) in chunks.iter().enumerate() {
        by_path.insert(c.symbol_path.clone(), i);
    }
    for i in 0..chunks.len() {
        if let Some(pp) = parent_path_of(&chunks[i].symbol_path) {
            if let Some(&pi) = by_path.get(&pp) {
                let pid = chunks[pi].id.clone();
                let entry = chunks[i].neighbors.get_or_insert_with(Default::default);
                entry.parent_id = Some(pid.clone());
                let pe = chunks[pi].neighbors.get_or_insert_with(Default::default);
                pe.children_ids.push(chunks[i].id.clone());
            }
        }
    }
}

fn parent_path_of(sym_path: &str) -> Option<String> {
    let parts: Vec<&str> = sym_path.split("::").collect();
    if parts.len() <= 2 {
        return None;
    }
    Some(parts[..parts.len() - 1].join("::"))
}

fn dedup_chunks(v: &mut Vec<CodeChunk>) {
    use std::collections::HashSet;
    let mut seen = HashSet::<(String, usize, usize, String)>::new();
    v.retain(|c| {
        seen.insert((
            c.file.clone(),
            c.span.start_byte,
            c.span.end_byte,
            c.symbol_path.clone(),
        ))
    });
}

fn read_ident(code: &str, n: Node) -> String {
    n.utf8_text(code.as_bytes()).unwrap_or_default().to_string()
}

fn read_ident_opt(code: &str, n: Node) -> Option<String> {
    Some(n.utf8_text(code.as_bytes()).ok()?.to_string())
}

fn collect_names_in_vdl(vdl: Node, code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut st = vec![vdl];
    while let Some(n) = st.pop() {
        match n.kind() {
            "identifier" | "simple_identifier" | "Identifier" | "SimpleIdentifier" => {
                let t = read_ident(code, n);
                if is_ident_like(&t) {
                    out.push(t);
                }
            }
            _ => {
                let mut w = n.walk();
                for ch in n.children(&mut w) {
                    st.push(ch);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|s| seen.insert(s.clone()));
    out
}

fn is_ident_like(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c == '_' || c == '$' || c.is_alphabetic() => true,
        _ => false,
    }
}

fn first_line(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch == '\n' {
            break;
        }
        out.push(ch);
        if out.len() >= max_chars {
            break;
        }
    }
    out.trim().to_string()
}

fn looks_generated(path: &str) -> bool {
    path.ends_with(".g.dart") || path.ends_with(".freezed.dart") || path.ends_with(".gr.dart")
}
