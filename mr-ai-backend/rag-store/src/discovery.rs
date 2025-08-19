//! Discovery utilities to find the latest dump directory and canonical file paths.

use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, trace};

/// Returns `<root>/project_x/graphs_data/<latest_timestamp>` directory.
pub fn latest_dump_dir(root: impl AsRef<Path>) -> Result<PathBuf, std::io::Error> {
    let p = root.as_ref().join("project_x/graphs_data");
    trace!("discovery::latest_dump_dir base={:?}", p);
    let mut best: Option<PathBuf> = None;
    if p.exists() {
        for entry in fs::read_dir(&p)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                if best
                    .as_ref()
                    .map(|b| name > b.file_name().unwrap())
                    .unwrap_or(true)
                {
                    best = Some(entry.path());
                }
            }
        }
    }
    let out =
        best.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no timestamp dir"))?;
    debug!("discovery::latest_dump_dir -> {:?}", out);
    Ok(out)
}

/// Canonical path to rag_records.jsonl inside a dump directory.
pub fn rag_records_path(dir: impl AsRef<Path>) -> PathBuf {
    dir.as_ref().join("rag_records.jsonl")
}

/// Minimal shape for `summary.json` that indexes produced files in the dump.
#[derive(Debug, Deserialize)]
pub struct DumpSummary {
    /// Absolute output directory of the dump (optional for our flow).
    pub out_dir: Option<String>,
    /// Mapping of logical names -> absolute file paths.
    pub files: HashMap<String, String>,
}

/// Reads `summary.json` located inside a dump directory.
pub fn read_dump_summary(dir: impl AsRef<Path>) -> Result<DumpSummary, std::io::Error> {
    let path = dir.as_ref().join("summary.json");
    trace!("discovery::read_dump_summary path={:?}", path);
    let data = fs::read_to_string(path)?;
    let summary: DumpSummary = serde_json::from_str(&data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(summary)
}
