use std::{fs, path::Path};

use graph_prepare::models::graph_node::GraphNode;
use qdrant_client::Payload;
use serde_json::json;
use services::uuid::stable_uuid;

use crate::models::VectorDoc;

/// Build a symbol-level document text (to be embedded).
/// Keeps a compact, structured header + optional code snippet.
pub fn symbol_doc_text(
    symbol_name: &str,
    kind: &str, // e.g., "class", "method", "function", ...
    file: &str,
    owner: Option<&str>,   // owner class for methods, if any
    snippet: Option<&str>, // optional code snippet
) -> String {
    let mut s = String::new();
    s.push_str(kind);
    s.push(' ');
    s.push_str(symbol_name);
    s.push('\n');

    if let Some(o) = owner {
        if !o.is_empty() {
            s.push_str("owner: ");
            s.push_str(o);
            s.push('\n');
        }
    }

    s.push_str("file: ");
    s.push_str(file);
    s.push('\n');

    if let Some(sn) = snippet {
        if !sn.is_empty() {
            s.push('\n');
            s.push_str(sn);
        }
    }

    s
}

/// Build a neighborhood summary text for a file-level document.
/// Lists imported files, declared symbols, and files that export this file.
pub fn neigh_text(
    file: &str,
    imports: &[String],
    declares: &[String],
    exported_by: &[String],
) -> String {
    let mut s = String::new();

    s.push_str("File: ");
    s.push_str(file);
    s.push('\n');

    if imports.is_empty() {
        s.push_str("Imports: (none)\n");
    } else {
        s.push_str("Imports: ");
        s.push_str(&imports.join(", "));
        s.push('\n');
    }

    if declares.is_empty() {
        s.push_str("Declares: (none)\n");
    } else {
        s.push_str("Declares: ");
        s.push_str(&declares.join(", "));
        s.push('\n');
    }

    if exported_by.is_empty() {
        s.push_str("Exported by: (none)\n");
    } else {
        s.push_str("Exported by: ");
        s.push_str(&exported_by.join(", "));
        s.push('\n');
    }

    s
}

/// Optional helper: load a code snippet from disk by 1-based [start, end] lines.
/// Returns None if file can't be read or indices are out of range.
pub fn load_snippet(file: &str, start_line: usize, end_line: usize) -> Option<String> {
    use std::fs;
    let ok_range = start_line > 0 && end_line >= start_line;
    if !ok_range {
        return None;
    }
    let content = fs::read_to_string(file).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let start_idx = start_line.saturating_sub(1);
    let end_idx = end_line.min(lines.len());
    if start_idx >= end_idx || start_idx >= lines.len() {
        return None;
    }
    Some(lines[start_idx..end_idx].join("\n"))
}

/// Read file as UTF-8 text; return None if unreadable or looks binary.
pub fn load_text(path: &str) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    // Heuristic: if more than 2% of bytes are zero, likely not text
    let nul = bytes.iter().filter(|b| **b == 0).count();
    if nul * 50 > bytes.len().max(1) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Map file extension to a language label (best-effort).
pub fn lang_from_ext(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
    {
        "dart" => "dart",
        "ts" => "typescript",
        "tsx" => "typescript",
        "js" => "javascript",
        "jsx" => "javascript",
        "py" => "python",
        "rs" => "rust",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "md" => "markdown",
        _ => "text",
    }
}

/// Append file-chunk docs for every `file` node found in graph_nodes.jsonl
pub async fn add_file_chunks(
    docs: &mut Vec<VectorDoc>,
    file_nodes: &[GraphNode],
    max_lines: usize,
    overlap: usize,
) {
    for n in file_nodes.iter().filter(|n| n.node_type == "file") {
        let Some(code) = load_text(&n.file) else {
            continue;
        };

        // Skip clearly generated/huge artifacts (optional hard guard)
        if code.len() > 2_000_000 {
            continue;
        } // 2 MB text cap

        let lang = lang_from_ext(&n.file);
        let chunks = chunk_by_lines(&code, max_lines, overlap);

        for (i, (s_line, e_line, body)) in chunks.into_iter().enumerate() {
            // Human-readable logical id
            let human_id = format!("file_chunk::{path}#{}", i + 1, path = n.file);

            // Stable UUIDv5 from human_id — Qdrant wants UUID or u64
            let point_id = stable_uuid(human_id.as_str()).to_string();

            // Text fed to the embedder: tiny header + the code itself
            let text = format!(
                "file: {path}\nlines: {s}-{e}\nlanguage: {lang}\n\n{body}",
                path = n.file,
                s = s_line,
                e = e_line,
                lang = lang,
                body = body
            );

            // Minimal payload to let you show exact location back to user
            let payload: Payload = json!({
                "source": "file_chunk",
                "file": n.file,
                "start_line": s_line as i64,
                "end_line": e_line as i64,
                "language": lang,
                "human_id": human_id, // useful for debugging
                "text": text,         // optional: for inspection
                "kind": "file_chunk",
            })
            .as_object()
            .unwrap()
            .clone()
            .into();

            docs.push(VectorDoc {
                id: point_id,
                text,
                payload,
            });
        }
    }
}

/// Simple line-based chunking to avoid tokenizers.
/// You may replace with a tokenizer-based splitter later.
pub fn chunk_by_lines(code: &str, max_lines: usize, overlap: usize) -> Vec<(usize, usize, String)> {
    let lines: Vec<&str> = code.lines().collect();
    if lines.is_empty() || max_lines == 0 {
        return vec![];
    }
    // guard against pathological overlap
    let step = if overlap >= max_lines {
        1
    } else {
        max_lines - overlap
    };

    let mut out = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + max_lines).min(lines.len());
        let text = lines[start..end].join("\n");
        out.push((start + 1, end, text));
        if end == lines.len() {
            break;
        }
        start += step;
    }
    out
}
