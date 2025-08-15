//! Streaming JSONL reader for `RagRecord`.

use crate::errors::RagError;
use crate::record::RagRecord;
use serde::ser::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use tracing::trace;

/// Reads and parses all `RagRecord` entries from a JSONL file.
///
/// This implementation loads records into memory. For very large datasets,
/// consider refactoring into a batched/streaming iterator.
///
/// # Errors
/// Returns `RagError::Io` or `RagError::Parse` on failures.
pub fn read_all_records(jsonl_path: impl AsRef<Path>) -> Result<Vec<RagRecord>, RagError> {
    trace!("io_jsonl::read_all_records path={:?}", jsonl_path.as_ref());
    let file = File::open(jsonl_path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: RagRecord = serde_json::from_str(&line).map_err(|e| {
            let msg = format!("line {} parse error: {}", i + 1, e);
            serde_json::Error::custom(msg)
        })?;
        out.push(rec);
    }
    trace!("io_jsonl::read_all_records parsed={} records", out.len());
    Ok(out)
}
