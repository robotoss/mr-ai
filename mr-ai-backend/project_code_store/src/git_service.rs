//! Universal Git service: bare clones + per-MR worktrees.
//!
//! Layout on disk (rooted at `GIT_CACHE_DIR`, default `code_data/git_cache`):
//!
//! ```text
//! code_data/
//! ├── git_cache/
//! │   └── <provider>/<owner>/<repo>.git/   ← bare clone, refreshed by fetch
//! └── worktrees/<job_id>/<repo>/           ← ephemeral working tree per MR
//! ```
//!
//! Bare clones are reused across jobs. Worktrees are linked from the bare
//! repo via `git worktree add` and removed via `git worktree remove` in the
//! `WorktreeHandle::cleanup` path. The legacy `clone_list` helper is kept
//! for ad-hoc tooling and tests; the `/sync_git` HTTP route was removed
//! in S5 in favour of `/admin/reindex_*`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use git2::build::RepoBuilder;
use git2::{FetchOptions, Repository};
use tokio::task;
use tracing::{debug, info, instrument, warn};

use crate::errors::{GitCloneError, Result};

/// Configuration for the Git service. Mirrors the env model from the docs.
#[derive(Debug, Clone)]
pub struct GitServiceConfig {
    pub git_cache_dir: PathBuf,
    pub worktree_dir: PathBuf,
}

impl GitServiceConfig {
    pub fn from_env() -> Self {
        let git_cache_dir = std::env::var("GIT_CACHE_DIR")
            .unwrap_or_else(|_| "code_data/git_cache".into())
            .into();
        let worktree_dir = std::env::var("WORKTREE_DIR")
            .unwrap_or_else(|_| "code_data/worktrees".into())
            .into();
        Self {
            git_cache_dir,
            worktree_dir,
        }
    }
}

/// Universal Git service handle. Cheap to clone.
#[derive(Debug, Clone)]
pub struct GitService {
    cfg: Arc<GitServiceConfig>,
}

impl GitService {
    pub fn new(cfg: GitServiceConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.git_cache_dir)?;
        std::fs::create_dir_all(&cfg.worktree_dir)?;
        Ok(Self { cfg: Arc::new(cfg) })
    }

    pub fn cache_root(&self) -> &Path {
        &self.cfg.git_cache_dir
    }

    pub fn worktree_root(&self) -> &Path {
        &self.cfg.worktree_dir
    }

    /// Compute the on-disk path for a bare clone keyed by remote URL.
    pub fn bare_path_for(&self, remote_url: &str) -> PathBuf {
        let key = sanitise_remote_for_path(remote_url);
        self.cfg.git_cache_dir.join(format!("{key}.git"))
    }

    /// Ensure the bare clone exists and is up to date. First call clones;
    /// subsequent calls just `git fetch`. Returns the bare path.
    #[instrument(skip(self), fields(remote = %remote_url))]
    pub async fn ensure_bare(&self, remote_url: &str) -> Result<PathBuf> {
        let path = self.bare_path_for(remote_url);
        let remote = remote_url.to_owned();
        let cloned = path.clone();
        task::spawn_blocking(move || ensure_bare_blocking(&remote, &cloned)).await??;
        Ok(path)
    }

    /// Create a `git worktree` from the bare clone for a specific ref.
    /// Returns a handle whose Drop removes the worktree directory and tells
    /// git to forget about it (`worktree prune`).
    #[instrument(skip(self), fields(remote = %remote_url, %git_ref, %job_tag))]
    pub async fn create_worktree(
        &self,
        remote_url: &str,
        git_ref: &str,
        job_tag: &str,
    ) -> Result<WorktreeHandle> {
        let bare = self.ensure_bare(remote_url).await?;
        let dir_name = sanitise_remote_for_path(remote_url);
        let target = self
            .cfg
            .worktree_dir
            .join(format!("{job_tag}-{dir_name}"));
        let target_for_blocking = target.clone();
        let bare_for_blocking = bare.clone();
        let git_ref_owned = git_ref.to_owned();
        task::spawn_blocking(move || {
            create_worktree_blocking(&bare_for_blocking, &target_for_blocking, &git_ref_owned)
        })
        .await??;

        info!(target = "git_service", path = %target.display(), "worktree ready");
        Ok(WorktreeHandle {
            bare,
            path: Some(target),
        })
    }
}

/// RAII guard for a created worktree. Drop removes the directory and
/// `worktree prune`s the bare clone. Failures are logged but never panic.
#[derive(Debug)]
pub struct WorktreeHandle {
    bare: PathBuf,
    path: Option<PathBuf>,
}

impl WorktreeHandle {
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Explicitly tear down the worktree. Drop calls this if you forget.
    pub fn cleanup(mut self) {
        if let Some(path) = self.path.take() {
            cleanup_worktree(&self.bare, &path);
        }
    }
}

impl Drop for WorktreeHandle {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            cleanup_worktree(&self.bare, &path);
        }
    }
}

fn cleanup_worktree(bare: &Path, path: &Path) {
    debug!(target = "git_service", path = %path.display(), "removing worktree");
    if let Err(err) = std::fs::remove_dir_all(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            warn!(target = "git_service", path = %path.display(), error = %err, "failed to remove worktree dir");
        }
    }
    // Tell git to forget the now-missing worktree.
    let bare_owned = bare.to_owned();
    let _ = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(&bare_owned)
        .args(["worktree", "prune"])
        .status();
}

fn ensure_bare_blocking(remote_url: &str, bare_path: &Path) -> Result<()> {
    if bare_path.exists() {
        debug!(target = "git_service", path = %bare_path.display(), "fetching existing bare");
        let repo = Repository::open_bare(bare_path)?;
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(make_credentials_cb());
        let mut fetch_opts = FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);
        let mut remote = repo
            .find_remote("origin")
            .or_else(|_| repo.remote_anonymous(remote_url))?;
        remote.fetch::<&str>(&[], Some(&mut fetch_opts), None)?;
        return Ok(());
    }

    // Fresh clone — bare.
    if let Some(parent) = bare_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    info!(target = "git_service", path = %bare_path.display(), "bare-cloning");

    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(make_credentials_cb());
    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);

    let mut builder = RepoBuilder::new();
    builder.bare(true).fetch_options(fetch_opts);
    builder.clone(remote_url, bare_path)?;
    Ok(())
}

fn create_worktree_blocking(bare: &Path, target: &Path, git_ref: &str) -> Result<()> {
    if target.exists() {
        std::fs::remove_dir_all(target)?;
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Use the git CLI for worktrees because git2 worktree support is coarse
    // and CLI semantics are stable across libgit2 versions.
    let status = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .args(["worktree", "add", "--detach"])
        .arg(target)
        .arg(git_ref)
        .status()
        .map_err(GitCloneError::Io)?;
    if !status.success() {
        return Err(GitCloneError::Git(git2::Error::from_str(&format!(
            "git worktree add failed for {} @ {git_ref}",
            target.display()
        ))));
    }
    Ok(())
}

fn make_credentials_cb()
-> impl FnMut(&str, Option<&str>, git2::CredentialType) -> std::result::Result<git2::Cred, git2::Error>
{
    move |url_str, username_from_url, allowed| {
        let user = username_from_url.unwrap_or("git");
        // S6: prefer host-scoped credentials so a worker fleet talking
        // to gitlab.com + github.example.com can keep distinct tokens
        // / SSH keys. Falls back to the unscoped key when no host-specific
        // value is configured.
        let host = secrets::host_from_remote_url(url_str);
        let host_ref = host.as_deref();
        if url_str.starts_with("http") {
            if let Some(token) =
                secrets::sync::resolve_with_host(None, host_ref, &secrets::SecretKey::GitHttpToken)
            {
                let http_user = secrets::sync::resolve_with_host(
                    None,
                    host_ref,
                    &secrets::SecretKey::GitHttpUser,
                )
                .unwrap_or_else(|| "oauth2".into());
                return git2::Cred::userpass_plaintext(&http_user, &token);
            }
        }
        if allowed.contains(git2::CredentialType::SSH_KEY) {
            if let Some(key) =
                secrets::sync::resolve_with_host(None, host_ref, &secrets::SecretKey::SshKeyPath)
            {
                let key_path = Path::new(&key);
                if key_path.exists() {
                    let pass = secrets::sync::resolve_with_host(
                        None,
                        host_ref,
                        &secrets::SecretKey::SshKeyPassphrase,
                    );
                    return git2::Cred::ssh_key(user, None, key_path, pass.as_deref());
                }
            }
            if let Ok(cred) = git2::Cred::ssh_key_from_agent(user) {
                return Ok(cred);
            }
        }
        if allowed.contains(git2::CredentialType::DEFAULT) {
            if let Ok(cred) = git2::Cred::default() {
                return Ok(cred);
            }
        }
        if allowed.contains(git2::CredentialType::USERNAME) {
            return git2::Cred::username(user);
        }
        Err(git2::Error::from_str("no usable credentials"))
    }
}

/// Map any remote URL to a stable filesystem-safe path:
///   git@gitlab.com:org/app.git → gitlab.com/org/app
///   https://github.com/org/app → github.com/org/app
pub fn sanitise_remote_for_path(remote_url: &str) -> String {
    let trimmed = remote_url.trim_end_matches('/').trim_end_matches(".git");
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .unwrap_or(trimmed);
    // SSH shorthand: git@host:org/repo
    let normalised = if let Some(rest) = without_scheme.strip_prefix("git@") {
        rest.replacen(':', "/", 1)
    } else {
        without_scheme.to_owned()
    };
    // Collapse consecutive slashes; replace anything path-unsafe with `_`.
    normalised
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.chars()
                .map(|c| match c {
                    'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' => c,
                    _ => '_',
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_ssh_shorthand() {
        assert_eq!(
            sanitise_remote_for_path("git@gitlab.com:org/app.git"),
            "gitlab.com/org/app"
        );
    }

    #[test]
    fn sanitise_https() {
        assert_eq!(
            sanitise_remote_for_path("https://github.com/org/app.git"),
            "github.com/org/app"
        );
    }

    #[test]
    fn sanitise_ssh_url_form() {
        assert_eq!(
            sanitise_remote_for_path("ssh://git@gitlab.com/org/app.git"),
            "gitlab.com/org/app"
        );
    }

    #[test]
    fn sanitise_strips_trailing_slash() {
        assert_eq!(
            sanitise_remote_for_path("https://example.com/x/"),
            "example.com/x"
        );
    }
}
