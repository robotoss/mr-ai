//! File-based cache for large diffs (JSON on disk).
//!
//! Why cache?
//! - Large MRs consume provider API limits and take time to parse.
//! - Re-running the pipeline on the same `head_sha` should be O(1).
//!
//! Key (stable across re-runs): SHA256("{provider}:{project}:{iid}:{head_sha}")
//! Layout: $MR_REVIEWER_CACHE_DIR/<provider>/<project_sanitized>/<iid>-<hash12>.json
//! Default cache dir: "code_data/mr_cache" (co-located with your project artifacts).

use crate::errors::MrResult;
use crate::git_providers::types::CrBundle;
use crate::git_providers::{ChangeRequestId, ProviderKind};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;

/// Returns the root directory for cache (env-overridable).
fn cache_root() -> PathBuf {
    std::env::var("MR_REVIEWER_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("code_data/mr_cache"))
}

/// Filesystem-safe replacement for project path (slashes → underscores).
fn sanitize(s: &str) -> String {
    s.replace('/', "_")
}

/// Computes deterministic cache path for the bundle.
fn key_path(kind: &ProviderKind, id: &ChangeRequestId, head_sha: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(format!("{:?}:{}:{}:{}", kind, id.project, id.iid, head_sha));
    let digest = format!("{:x}", hasher.finalize());
    cache_root()
        .join(match kind {
            ProviderKind::GitLab => "gitlab",
            ProviderKind::GitHub => "github",
            ProviderKind::Bitbucket => "bitbucket",
        })
        .join(sanitize(&id.project))
        .join(format!("{}-{}.json", id.iid, &digest[..12]))
}

/// Loads bundle from cache if present.
pub async fn load_bundle(
    kind: &ProviderKind,
    id: &ChangeRequestId,
    head_sha: &str,
) -> MrResult<Option<CrBundle>> {
    let path = key_path(kind, id, head_sha);
    if !Path::new(&path).exists() {
        return Ok(None);
    }
    let data = fs::read(&path).await?;
    let bundle: CrBundle = serde_json::from_slice(&data)?;
    Ok(Some(bundle))
}

/// Stores bundle if considered "large".
///
/// Heuristics:
/// - many files (e.g. > 200)
/// - huge raw unified diff bytes (e.g. > 5 MiB)
/// - provider flagged truncation (is_truncated=true)
pub async fn maybe_store_bundle(
    kind: &ProviderKind,
    id: &ChangeRequestId,
    head_sha: &str,
    bundle: &CrBundle,
) -> MrResult<()> {
    let files = bundle.changes.files.len();
    let bytes: usize = bundle
        .changes
        .files
        .iter()
        .filter_map(|f| f.raw_unidiff.as_ref())
        .map(|s| s.len())
        .sum();
    let is_large = files > 200 || bytes > 5 * 1024 * 1024 || bundle.changes.is_truncated;
    if !is_large {
        return Ok(());
    }

    let path = key_path(kind, id, head_sha);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).await?;
    }
    let json = serde_json::to_vec(bundle)?;
    fs::write(path, json).await?;
    Ok(())
}
