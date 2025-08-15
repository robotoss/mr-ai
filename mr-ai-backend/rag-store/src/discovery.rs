//! Timestamp discovery utilities for dumps under `project_x/graphs_data`.

use std::path::{Path, PathBuf};

use tracing::trace;

/// Returns the latest timestamp directory under `<root>/project_x/graphs_data`.
///
/// The format `YYYYMMDD_HHMMSS` is lexicographically sortable, so a simple sort is sufficient.
///
/// # Errors
/// Returns `std::io::Error` if the directory cannot be read or is empty.
pub fn latest_dump_dir(root: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let graphs_dir = root.as_ref().join("project_x").join("graphs_data");
    trace!("discovery::latest_dump_dir scanning {:?}", graphs_dir);

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&graphs_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();

    dirs.sort(); // YYYYMMDD_HHMMSS works with lexicographic order
    let last = dirs
        .pop()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no timestamp folders"))?;
    trace!("discovery::latest_dump_dir found {:?}", last);
    Ok(last)
}

/// Builds a path to `rag_records.jsonl` inside a discovered dump directory.
pub fn rag_records_path(dir: impl AsRef<Path>) -> PathBuf {
    let p = dir.as_ref().join("rag_records.jsonl");
    trace!("discovery::rag_records_path -> {:?}", p);
    p
}
