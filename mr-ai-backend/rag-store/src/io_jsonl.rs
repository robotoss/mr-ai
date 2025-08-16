//! JSONL helpers: strict RagRecord reader and generic Value reader.
//!
//! Provides two utilities:
//! - [`read_all_records`] → strict parsing into [`RagRecord`] (requires `id` + `text`).
//! - [`read_all_jsonl`] → tolerant parsing into raw [`serde_json::Value`].

use crate::errors::RagError;
use crate::record::RagRecord;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::io::{BufRead, BufReader};
use std::{fs::File, path::Path};
use tracing::{debug, info, warn};

/// Internal row shape used by [`read_all_records`].
///
/// This mirrors the expected JSONL schema for RAG ingestion:
/// - `id`: unique record identifier
/// - `text`: main content
/// - `source`: optional origin (file, URI, etc.)
/// - `embedding`: optional pre-computed embedding
/// - `extra`: optional metadata map
#[derive(Deserialize)]
struct StrictRow {
    id: String,
    text: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    embedding: Option<Vec<f32>>,
    #[serde(default)]
    extra: Option<Map<String, Value>>,
}

/// Reads RagRecord JSONL strictly.
///
/// - Expects at least `id` and `text`.
/// - Fails on malformed rows with [`RagError::Parse`].
/// - Ignores empty lines.
///
/// # Errors
/// - [`RagError::Io`] if the file cannot be read.
/// - [`RagError::Parse`] if any line fails strict deserialization.
pub fn read_all_records(jsonl_path: impl AsRef<Path>) -> Result<Vec<RagRecord>, RagError> {
    info!("Reading strict RagRecord JSONL: {:?}", jsonl_path.as_ref());

    let file = File::open(jsonl_path.as_ref())?;
    let reader = BufReader::new(file);

    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let r: StrictRow = serde_json::from_str(&line)
            .map_err(|e| RagError::Parse(format!("line {} parse error: {}", i + 1, e)))?;

        let extra = r
            .extra
            .unwrap_or_default()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();

        out.push(RagRecord {
            id: r.id,
            text: r.text,
            source: r.source,
            embedding: r.embedding,
            extra,
        });
    }

    debug!("Loaded {} strict RagRecords", out.len());
    Ok(out)
}

/// Reads arbitrary JSONL into a vector of [`serde_json::Value`].
///
/// This reader is **tolerant**:
/// - Empty lines are skipped.
/// - Malformed lines are logged (`warn!`) but not fatal.
///
/// # Errors
/// - [`RagError::Io`] if the file cannot be opened.
pub fn read_all_jsonl(jsonl_path: impl AsRef<Path>) -> Result<Vec<Value>, RagError> {
    info!("Reading generic JSONL: {:?}", jsonl_path.as_ref());

    let file = File::open(jsonl_path)?;
    let reader = BufReader::new(file);

    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<Value>(&line) {
            Ok(v) => out.push(v),
            Err(e) => {
                warn!("Skipping malformed JSON on line {}: {}", i + 1, e);
            }
        }
    }

    debug!("Loaded {} generic JSON values", out.len());
    Ok(out)
}
