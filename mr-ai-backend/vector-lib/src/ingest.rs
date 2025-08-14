use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use graph_prepare::models::{ast_node::ASTNode, graph_node::GraphNode};
use qdrant_client::{Payload, qdrant::PointId};
use serde::Deserialize;
use serde_json::json;
use services::uuid::stable_uuid;

use crate::{
    chunk::{add_file_chunks, neigh_text, symbol_doc_text},
    models::VectorDoc,
    ollama::OllamaEmb,
    qdrant::QdrantStore,
};

/// Edge structure from graph_edges.jsonl
#[derive(Debug, Deserialize)]
struct GraphEdge {
    src: usize,
    dst: usize,
    label: String, // declares | imports | exports | part | imports_via_export ...
}

/// Find the latest export folder inside `graphs_data`
fn latest_graphs_dir(repo_root: &str, export_dir_name: &str) -> Result<PathBuf> {
    let base = Path::new(repo_root).join(export_dir_name);
    let mut entries = std::fs::read_dir(&base)
        .with_context(|| format!("read_dir {}", base.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .collect::<Vec<_>>();
    entries.sort_by_key(|e| e.file_name());
    let last = entries.pop().context("graphs_data is empty")?;
    Ok(last.path())
}

/// Read a JSONL file into a Vec<T>
fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let rdr = BufReader::new(f);
    let mut out = Vec::new();
    for (i, line) in rdr.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let val: T = serde_json::from_str(&line)
            .with_context(|| format!("parse {} line {}", path.display(), i + 1))?;
        out.push(val);
    }
    Ok(out)
}

/// Build vector docs from AST/graph exports and send them to Qdrant
pub async fn ingest_from_graph_prepare(
    repo_root: &str,
    out_dir: Option<&str>, // if None -> use latest timestamp in graphs_data
    q: &QdrantStore,
    collection: &str,
    emb: &OllamaEmb,
) -> Result<()> {
    // 1) Locate export directory
    let graphs_dir = match out_dir {
        Some(p) => PathBuf::from(p),
        None => latest_graphs_dir(
            repo_root,
            &std::env::var("GRAPH_EXPORT_DIR_NAME").unwrap_or_else(|_| "graphs_data".into()),
        )?,
    };

    let nodes_path = graphs_dir.join("graph_nodes.jsonl");
    let edges_path = graphs_dir.join("graph_edges.jsonl");

    // 2) Load AST nodes and edges
    let nodes: Vec<GraphNode> = read_jsonl(&nodes_path)?;
    let edges: Vec<GraphEdge> = read_jsonl(&edges_path)?;

    let mut node_by_id: HashMap<usize, &GraphNode> = HashMap::new();
    for n in &nodes {
        node_by_id.insert(n.id, n);
    }

    // Collect neighborhood info for each file
    #[derive(Default, Clone)]
    struct FileNeigh {
        imports: HashSet<String>,
        declares: HashSet<String>,
        exported_by: HashSet<String>,
    }
    let mut neigh: HashMap<usize, FileNeigh> = HashMap::new();

    // read env or use sane defaults
    let max_lines: usize = std::env::var("CHUNK_MAX_LINES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let overlap: usize = std::env::var("CHUNK_OVERLAP_LINES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    // Keep a local copy of file nodes (we already have `nodes: Vec<GraphNode>`)
    let file_nodes: Vec<GraphNode> = nodes
        .iter()
        .filter(|n| n.node_type == "file")
        .cloned()
        .collect();

    let is_file = |n: &GraphNode| n.node_type == "file";
    let is_symbol = |n: &GraphNode| {
        matches!(
            n.node_type.as_str(),
            "class"
                | "method"
                | "function"
                | "enum"
                | "extension"
                | "mixin"
                | "interface"
                | "struct"
        )
    };

    for e in &edges {
        let Some(&src) = node_by_id.get(&e.src) else {
            continue;
        };
        let Some(&dst) = node_by_id.get(&e.dst) else {
            continue;
        };

        match e.label.as_str() {
            "declares" if is_file(src) && !is_file(dst) => {
                neigh
                    .entry(src.id)
                    .or_default()
                    .declares
                    .insert(dst.name.clone());
            }
            "imports" | "imports_via_export" if is_file(src) && is_file(dst) => {
                neigh
                    .entry(src.id)
                    .or_default()
                    .imports
                    .insert(dst.file.clone());
            }
            "exports" if is_file(src) && is_file(dst) => {
                // file `src` exports file `dst` -> значит `dst` exported_by `src`
                neigh
                    .entry(dst.id)
                    .or_default()
                    .exported_by
                    .insert(src.file.clone());
            }
            _ => {}
        }
    }

    // 3) Build VectorDocs
    let mut docs: Vec<VectorDoc> = Vec::new();

    // Append file chunks
    add_file_chunks(&mut docs, &file_nodes, max_lines, overlap).await;

    // Symbol documents
    for (i, n) in nodes.iter().enumerate() {
        if !is_symbol(n) {
            continue;
        }
        let id = format!(
            "sym::{kind}::{file}::{name}",
            kind = n.node_type,
            file = n.file,
            name = n.name
        );
        let text = symbol_doc_text(
            &n.name,
            &n.node_type,
            &n.file,
            None,
            None, // you can add snippet extraction later
        );

        let mut payload: Payload = Default::default();
        payload.insert("source".to_string(), json!("symbol"));
        payload.insert("text".to_string(), json!(text));
        payload.insert("file".to_string(), json!(n.file));
        payload.insert("kind".to_string(), json!(n.node_type));
        payload.insert("name".to_string(), json!(n.name));
        payload.insert("start_line".to_string(), json!(n.start_line as i64));
        payload.insert("end_line".to_string(), json!(n.end_line as i64));

        docs.push(VectorDoc { id, text, payload });
    }

    // Neighborhood documents
    for (i, n) in nodes.iter().enumerate() {
        if !is_file(n) {
            continue;
        }
        let nb = neigh.get(&i).cloned().unwrap_or_default();
        let imports: Vec<String> = nb.imports.into_iter().collect();
        let declares: Vec<String> = nb.declares.into_iter().collect();
        let exported_by: Vec<String> = nb.exported_by.into_iter().collect();

        let text = neigh_text(&n.file, &imports, &declares, &exported_by);
        let id = format!("neigh::{file}", file = n.file);

        let mut payload: Payload = Default::default();
        payload.insert("source".to_string(), json!("neighborhood"));
        payload.insert("text".to_string(), json!(text));
        payload.insert("file".to_string(), json!(n.file));
        payload.insert("imports".to_string(), json!(imports));
        payload.insert("declares".to_string(), json!(declares));
        payload.insert("exported_by".to_string(), json!(exported_by));

        docs.push(VectorDoc { id, text, payload });
    }

    // 4) Send to Qdrant
    ingest_docs(q, collection, emb, docs).await?;

    Ok(())
}

/// Batch-embed documents and upsert them into Qdrant.
/// Uses ENV QDRANT_BATCH_SIZE (default 32).
pub async fn ingest_docs(
    q: &QdrantStore,
    collection: &str,
    emb: &OllamaEmb,
    docs: Vec<VectorDoc>,
) -> Result<()> {
    // Keep batches modest to avoid overloading Ollama/Qdrant
    let batch: usize = std::env::var("QDRANT_BATCH_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(32);

    for chunk in docs.chunks(batch) {
        // 1) Collect texts for embedding
        let texts: Vec<String> = chunk.iter().map(|d| d.text.clone()).collect();

        // 2) Compute embeddings
        let vectors: Vec<Vec<f32>> = emb.embed_batch(&texts).await?;

        // 3) Prepare IDs and payloads (must match lengths)
        let ids: Vec<String> = chunk
            .iter()
            .map(|d| stable_uuid(&d.id).to_string())
            .collect();

        let payloads: Vec<Payload> = chunk.iter().map(|d| d.payload.clone()).collect();

        // 4) Upsert into Qdrant (wait for durability)
        q.upsert_points_wait(collection, ids, vectors, payloads)
            .await?;
    }

    Ok(())
}
