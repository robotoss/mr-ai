/// Simple line-based chunking to avoid tokenizers.
/// You may replace with a tokenizer-based splitter later.
pub fn chunk_by_lines(code: &str, max_lines: usize, overlap: usize) -> Vec<(usize, usize, String)> {
    let lines: Vec<&str> = code.lines().collect();
    if lines.is_empty() {
        return vec![];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + max_lines).min(lines.len());
        let text = lines[start..end].join("\n");
        out.push((start + 1, end, text));
        if end == lines.len() {
            break;
        }
        start = end.saturating_sub(overlap);
    }
    out
}
