//! File logging helpers for pre-question results.
//! All files are stored under code_data/mr_tmp/<sha>/preq/<idx>/

use serde::Serialize;
use std::fs;
use std::path::PathBuf;

pub fn write_raw(head_sha: &str, idx: usize, name: &str, data: &str) {
    if let Err(e) = write_bytes(head_sha, idx, name, data.as_bytes()) {
        tracing::warn!("preq.log: failed to write {}: {}", name, e);
    }
}

pub fn write_json<T: Serialize>(head_sha: &str, idx: usize, name: &str, value: &T) {
    match serde_json::to_vec_pretty(value) {
        Ok(bytes) => {
            if let Err(e) = write_bytes(head_sha, idx, name, &bytes) {
                tracing::warn!("preq.log: failed to write {}: {}", name, e);
            }
        }
        Err(e) => tracing::warn!("preq.log: failed to serialize {}: {}", name, e),
    }
}

fn write_bytes(head_sha: &str, idx: usize, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let short = if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    };
    let dir = PathBuf::from("code_data")
        .join("mr_tmp")
        .join(short)
        .join("preq")
        .join(format!("{}", idx));
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(name), bytes)?;
    Ok(())
}
