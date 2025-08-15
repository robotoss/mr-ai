//! Qdrant payload preparation.
//!
//! We do **not** compute embeddings here. This module just writes the `RagRecord`
//! JSONLs that a later vectorization step will read, embed, and upsert to Qdrant.

use crate::model::payload::RagRecord;
use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
use tracing::info;

/// Write RAG records (as-is) to JSONL for Qdrant ingestion.
pub fn write_qdrant_payload_jsonl(path: &Path, records: &[RagRecord]) -> Result<()> {
    let f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut w = BufWriter::new(f);
    for r in records {
        serde_json::to_writer(&mut w, r)?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    info!("qdrant_prep: wrote payload -> {}", path.display());
    Ok(())
}
