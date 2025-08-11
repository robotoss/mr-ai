use graph_prepare::models::ast_node::ASTNode;

pub struct Chunk {
    pub id: String,
    pub text: String,
    pub file: String,
    pub node_type: String, // "file","class","function","method"
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// Build chunks primarily from function/class/method nodes; fallback to per-file chunks.
pub fn make_chunks_from_nodes_with_limits(
    nodes: &[ASTNode],
    read_file: &dyn Fn(&str) -> Option<String>,
    max_chars: usize,
    min_chars: usize,
) -> Vec<Chunk> {
    let mut out = Vec::new();

    // symbol-level
    for n in nodes {
        if n.node_type == "function" || n.node_type == "class" || n.node_type == "method" {
            if let Some(src) = read_file(&n.file) {
                let mut snippet = slice_lines(&src, n.start_line, n.end_line);
                if max_chars > 0 && snippet.len() > max_chars {
                    snippet.truncate(max_chars);
                }
                if snippet.len() >= min_chars {
                    let id = format!("sym|{}|{}|{}:{}", n.file, n.node_type, n.name, n.start_line);
                    out.push(Chunk {
                        id,
                        text: snippet,
                        file: n.file.clone(),
                        node_type: n.node_type.clone(),
                        name: n.name.clone(),
                        start_line: n.start_line,
                        end_line: n.end_line,
                    });
                }
            }
        }
    }

    // per-file fallback
    use std::collections::HashSet;
    let mut seen_file = HashSet::new();
    for n in nodes {
        if seen_file.insert(n.file.clone()) {
            let has_symbol_chunk = out.iter().any(|c| c.file == n.file);
            if !has_symbol_chunk {
                if let Some(mut src) = read_file(&n.file) {
                    if max_chars > 0 && src.len() > max_chars {
                        src.truncate(max_chars);
                    }
                    if src.len() >= min_chars {
                        let id = format!("file|{}", n.file);
                        out.push(Chunk {
                            id,
                            text: src.clone(),
                            file: n.file.clone(),
                            node_type: "file".to_string(),
                            name: n.file.clone(),
                            start_line: 1,
                            end_line: src.lines().count(),
                        });
                    }
                }
            }
        }
    }
    out
}

fn slice_lines(src: &str, start: usize, end: usize) -> String {
    let mut out = String::new();
    for (i, line) in src.lines().enumerate() {
        let ln = i + 1;
        if ln >= start && ln <= end {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
