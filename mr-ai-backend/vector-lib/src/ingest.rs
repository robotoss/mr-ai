use crate::{models::VectorDoc, ollama::OllamaEmb, qdrant::QdrantStore};
use anyhow::Result;
use qdrant_client::Payload;

pub async fn ingest_docs(
    q: &QdrantStore,
    coll: &str,
    emb: &OllamaEmb,
    docs: Vec<VectorDoc>,
) -> Result<()> {
    // Batch in modest sizes to not overload Ollama/Qdrant
    let batch = 32;
    for chunk in docs.chunks(batch) {
        let texts: Vec<String> = chunk.iter().map(|d| d.text.clone()).collect();
        let vecs = emb.embed_batch(&texts).await?;
        let ids: Vec<String> = chunk.iter().map(|d| d.id.clone()).collect();
        let payloads: Vec<Payload> = chunk.iter().map(|d| d.payload.clone()).collect();
        q.upsert_points_wait(coll, ids, vecs, payloads).await?;
    }
    Ok(())
}

/// Build a simple symbol doc text from AST.
pub fn symbol_doc_text(
    symbol_name: &str,
    kind: &str,
    file: &str,
    owner: Option<&str>,
    snippet: Option<&str>,
) -> String {
    let mut s = String::new();
    s.push_str(kind);
    s.push(' ');
    s.push_str(symbol_name);
    s.push('\n');
    if let Some(o) = owner {
        s.push_str(&format!("owner: {}\n", o));
    }
    s.push_str(&format!("file: {}\n", file));
    if let Some(sn) = snippet {
        s.push_str("\n");
        s.push_str(sn);
    }
    s
}

/// Build a neighborhood summary text.
pub fn neigh_text(
    file: &str,
    imports: &[String],
    declares: &[String],
    exported_by: &[String],
) -> String {
    format!(
        "File: {file}\nImports: {}\nDeclares: {}\nExported by: {}",
        imports.join(", "),
        declares.join(", "),
        exported_by.join(", "),
    )
}
