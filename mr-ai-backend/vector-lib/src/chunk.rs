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
