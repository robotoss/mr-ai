pub mod fs_scan;
pub mod jsonl;
pub mod microchunk;

use crate::errors::{Error, Result};
use std::path::Path;

/// Ensure directory exists; create recursively if missing.
pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(Error::from)
}
