//! Async Git cloning utilities built on top of **gix** (gitoxide).
//!
//! ## Features
//! - Concurrent cloning of multiple repositories with a configurable concurrency limit.
//! - SSH and HTTPS URLs supported (relies on your system `ssh` agent/`~/.ssh/config`).
//! - Repositories are placed under `code_data/{project_name}/{repo_name}`.
//! - Target directory is cleaned (removed) per repository before cloning.
//!
//! ## Design
//! `gix` exposes a blocking cloning API. To integrate smoothly with async code,
//! each clone runs inside `tokio::task::spawn_blocking`, while a `tokio::sync::Semaphore`
//! bounds the number of concurrent clones.
//!
//! ## Logging
//! Uses `tracing` (`info!`, `warn!`, `debug!`, `error!`) with `#[instrument]` spans.
//! To initialize logging in your binary, for example:
//! ```ignore
//! use tracing_subscriber::{fmt, EnvFilter};
//!
//! tracing_subscriber::fmt()
//!     .with_env_filter(EnvFilter::from_default_env())
//!     .compact()
//!     .init();
//! ```
//!
//! ## Cargo features (important)
//! Make sure `gix` is compiled with:
//! - `blocking-network-client`
//! - `worktree-mutation`
//!
//! Example `Cargo.toml` rows:
//! ```toml
//! gix = { version = "0.73", default-features = false, features = ["blocking-network-client", "worktree-mutation"] }
//! tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync"] }
//! tracing = "0.1"
//! thiserror = "1"
//! ```
//!
//! ## Example
//! ```ignore
//! // In your async context:
//! let urls = vec![
//!     "git@github.com:owner/repo1.git".to_string(),
//!     "https://gitlab.com/group/repo2.git".to_string(),
//! ];
//! git_clone::clone_list(urls, 4, "project_x".to_string()).await?;
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

mod errors;

use tokio::{sync::Semaphore, task};
use tracing::{debug, error, info, instrument, warn};

use errors::{GitCloneError, Result};

/// Clone multiple repositories concurrently (bounded by `max_concurrency`).
///
/// Repositories are placed under `code_data/{project_name}/{repo_name}`.
/// The function ensures the base directory exists and then spawns a blocking
/// task per repository clone, bounded by a semaphore.
///
/// ### Parameters
/// - `urls`: List of Git repository URLs (SSH or HTTPS).
/// - `max_concurrency`: Maximum number of clones to run in parallel (minimum 1).
/// - `project_name`: Human-friendly project name used to build the target base path.
///
/// ### Returns
/// - `Ok(())` on success (all clones completed).
/// - `Err(GitCloneError)` if any clone or FS operation fails (fails fast).
///
/// ### Notes
/// - Each target repo directory is **removed** before cloning to avoid dirty states.
/// - SSH authentication is handled by your system `ssh` setup and/or agent.
///
/// ### Example
/// ```ignore
/// git_clone::clone_list(vec!["git@github.com:owner/repo.git".into()], 2, "demo".into()).await?;
/// ```
#[instrument(skip_all, fields(project = %project_name, max = max_concurrency, total = urls.len()))]
pub async fn clone_list(
    urls: Vec<String>,
    max_concurrency: usize,
    project_name: String,
) -> Result<()> {
    let base_dir = PathBuf::from(format!("code_data/{project_name}"));
    ensure_dir(&base_dir)?;

    let sem = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let mut tasks = Vec::with_capacity(urls.len());

    for url in urls {
        let base_dir = base_dir.clone();
        let permit = sem.clone().acquire_owned().await.unwrap();

        // Heavy I/O runs on a blocking thread to avoid stalling the async runtime.
        tasks.push(task::spawn_blocking(move || {
            let _span = tracing::info_span!("clone_task", repo = %url).entered();
            let res = clone_one_blocking(&url, &base_dir);
            drop(permit);
            res
        }));
    }

    // Wait for all tasks, returning the first error encountered (if any).
    for t in tasks {
        t.await??;
    }

    info!("all clones finished");
    Ok(())
}

/// **Blocking** single-repo clone that runs inside `spawn_blocking`.
///
/// This performs the canonical gix cloning sequence:
/// 1) `prepare_clone()`
/// 2) `fetch_then_checkout()`
/// 3) `checkout.main_worktree()`
///
/// The target directory is removed beforehand if it already exists.
///
/// ### Parameters
/// - `url`: Git repository URL (SSH or HTTPS).
/// - `base_dir`: Base directory under which the `<repo_name>` folder will be created.
///
/// ### Returns
/// - `Ok(())` on success.
/// - `Err(GitCloneError)` on failure.
///
/// ### Panics
/// - Does **not** panic; errors are propagated as `GitCloneError`.
#[instrument(skip(base_dir), fields(repo = %url))]
fn clone_one_blocking(url: &str, base_dir: &Path) -> Result<()> {
    info!("start clone");

    let repo_name = extract_repo_name(url).unwrap_or_else(|| "unnamed_repo".into());
    let target = base_dir.join(&repo_name);
    debug!(%repo_name, path = %target.display(), "resolved target dir");

    if target.exists() {
        warn!(path = %target.display(), "removing existing target");
        fs::remove_dir_all(&target)?;
    }

    // gix clone: prepare → fetch_then_checkout → main_worktree
    let abort = AtomicBool::new(false);

    let (mut checkout, _outcome) = gix::prepare_clone(url, &target)
        .map_err(|e| {
            error!(error = %e, "prepare_clone failed");
            GitCloneError::Git(format!("prepare_clone: {e}"))
        })?
        .fetch_then_checkout(gix::progress::Discard, &abort)
        .map_err(|e| {
            error!(error = %e, "fetch_then_checkout failed");
            GitCloneError::Git(format!("fetch_then_checkout: {e}"))
        })?;

    checkout
        .main_worktree(gix::progress::Discard, &abort)
        .map_err(|e| {
            error!(error = %e, "checkout worktree failed");
            GitCloneError::Git(format!("checkout worktree: {e}"))
        })?;

    info!(path = %target.display(), "clone completed");
    Ok(())
}

/// Extract the repository folder name from common Git URL forms.
///
/// Supports:
/// - `https://host/org/repo.git`
/// - `ssh://git@host/org/repo.git`
/// - `git@host:org/repo.git`
///
/// Trailing slashes and `.git` suffix are removed.
///
/// ### Examples
/// ```rust
/// # use crate::git_clone::mod as _; // doc-only placeholder
/// let name = super::extract_repo_name("git@github.com:org/repo.git").unwrap();
/// assert_eq!(name, "repo");
/// ```
#[instrument(level = "trace", skip_all, fields(url = %url))]
fn extract_repo_name(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let last = if let Some(i) = trimmed.rfind('/') {
        &trimmed[i + 1..]
    } else if let Some(i) = trimmed.rfind(':') {
        &trimmed[i + 1..]
    } else {
        trimmed
    };
    let name = last.trim_end_matches(".git").to_string();
    debug!(%name, "extracted repo name");
    Some(name)
}

/// Ensure the given directory exists (create it if necessary).
///
/// Returns `Ok(())` if the directory already exists or was created successfully.
///
/// ### Errors
/// - Returns `GitCloneError::Io` if directory creation fails.
///
/// ### Example
/// ```ignore
/// ensure_dir(std::path::Path::new("code_data/project_x"))?;
/// ```
#[instrument(level = "trace", skip_all, fields(path = %dir.display()))]
fn ensure_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        info!(path = %dir.display(), "created base dir");
    } else {
        debug!(path = %dir.display(), "base dir exists");
    }
    Ok(())
}
