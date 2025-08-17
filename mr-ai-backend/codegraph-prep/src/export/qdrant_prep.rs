//! Qdrant payload preparation.
//!
//! We do not compute embeddings here. This module only writes `RagRecord`
//! JSONL that a later step will embed and upsert into Qdrant.

use crate::model::payload::RagRecord;
use anyhow::{Context, Result};
use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};
use tracing::info;

/// Write RAG records (as-is) to JSONL for Qdrant ingestion.
/// Returns the number of records written.
pub fn write_qdrant_payload_jsonl(path: &Path, records: &[RagRecord]) -> Result<usize> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }

    // Create file and wrap in a buffered writer
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut w = BufWriter::new(file);

    // Stream NDJSON: one JSON object per line
    for (i, r) in records.iter().enumerate() {
        serde_json::to_writer(&mut w, r)
            .with_context(|| format!("serialize record #{i} id={}", r.id))?;
        w.write_all(b"\n").context("write newline")?;
    }

    // Flush buffers and fsync the file for durability
    w.flush().context("flush writer")?;
    // Convert back into File to sync. If into_inner fails, include the partial write error.
    let file = w.into_inner().context("finalize writer")?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))?;

    info!(
        "qdrant_prep: wrote {} records -> {}",
        records.len(),
        path.display()
    );
    Ok(records.len())
}
