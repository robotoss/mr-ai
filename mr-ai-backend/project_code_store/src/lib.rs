//! Async Git cloning utilities built on `git2` (libgit2).
//!
//! - Concurrency via `tokio::Semaphore` + `spawn_blocking`.
//! - SSH auth: `SSH_KEY_PATH` (private key) or ssh-agent fallback.
//! - HTTPS auth: `GIT_HTTP_TOKEN` (+ `GIT_HTTP_USER`, default `oauth2`).
//! - Repos are cloned to `code_data/{project_name}/{repo_name}`; target dir removed if exists.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use git2::{Cred, CredentialType, FetchOptions, RemoteCallbacks, build::RepoBuilder};
use tokio::{sync::Semaphore, task};
use tracing::{debug, error, info, instrument, warn};

pub mod errors;
use errors::Result;

/// Clone multiple repositories concurrently (bounded by `max_concurrency`).
///
/// Target path for each repo: `code_data/{project_name}/{repo_name}`.
/// The per-repo directory is removed before cloning.
#[instrument(skip_all, fields(project = %project_name, max = max_concurrency, total = urls.len()))]
pub async fn clone_list(
    urls: Vec<String>,
    max_concurrency: usize,
    project_name: &String,
) -> Result<()> {
    let base_dir = PathBuf::from(format!("code_data/{project_name}"));
    ensure_dir(&base_dir)?;

    let sem = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let mut tasks = Vec::with_capacity(urls.len());

    for url in urls {
        let base_dir = base_dir.clone();
        let permit = sem.clone().acquire_owned().await.unwrap();

        tasks.push(task::spawn_blocking(move || {
            let _span = tracing::info_span!("clone_task", repo = %url).entered();
            let res = clone_one_blocking(&url, &base_dir);
            drop(permit);
            res
        }));
    }

    for t in tasks {
        t.await??;
    }

    info!("all clones finished");
    Ok(())
}

/// Blocking clone (runs inside `spawn_blocking`).
///
/// - Creates/cleans `<base_dir>/<repo_name>`.
/// - Configures libgit2 credential callbacks for SSH/HTTPS.
/// - Clones with `RepoBuilder`.
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

    // --- credentials callback ---
    let key_path_env = std::env::var("SSH_KEY_PATH").ok();
    let key_path_disk = Path::new("ssh_keys/bot_key");
    let have_disk_key = key_path_disk.exists();

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url_str, username_from_url, allowed| {
        let user = username_from_url.unwrap_or("git");

        // HTTPS with token from env (optional)
        if url_str.starts_with("http") {
            if let Ok(token) = std::env::var("GIT_HTTP_TOKEN") {
                let http_user = std::env::var("GIT_HTTP_USER").unwrap_or_else(|_| "oauth2".into());
                return Cred::userpass_plaintext(&http_user, &token);
            }
        }

        // Prefer explicit SSH key path from env
        if allowed.contains(CredentialType::SSH_KEY) {
            if let Some(ref key) = key_path_env {
                let key_path = Path::new(key);
                if key_path.exists() {
                    let pass = std::env::var("SSH_KEY_PASSPHRASE").ok();
                    return Cred::ssh_key(user, None, key_path, pass.as_deref());
                }
            }
            // fallback: ./ssh_keys/bot_key
            if have_disk_key {
                let pass = std::env::var("SSH_KEY_PASSPHRASE").ok();
                return Cred::ssh_key(user, None, key_path_disk, pass.as_deref());
            }
        }

        // Try ssh-agent
        if allowed.contains(CredentialType::SSH_KEY) {
            if let Ok(cred) = Cred::ssh_key_from_agent(user) {
                return Ok(cred);
            }
        }

        // libgit2 default creds (netrc/manager, etc.)
        if allowed.contains(CredentialType::DEFAULT) {
            if let Ok(cred) = Cred::default() {
                return Ok(cred);
            }
        }

        // If server asked only username, provide it
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(user);
        }

        Err(git2::Error::from_str("no usable credentials"))
    });

    // You *may* want to relax TLS/host checks, but better keep defaults.
    // callbacks.certificate_check(|_cert, _host| Ok(())); // <- not recommended for prod

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);

    let mut builder = RepoBuilder::new();
    builder.fetch_options(fetch_opts);

    // Shallow clone example (optional):
    // use git2::RepositoryInitOptions;
    // fetch_opts.download_tags(git2::AutotagOption::All);
    // builder.branch("main"); // checkout 'main'

    info!(path = %target.display(), "begin clone");
    match builder.clone(url, &target) {
        Ok(_) => {
            info!(path = %target.display(), "clone completed");
            Ok(())
        }
        Err(e) => {
            error!(error = %e, "clone failed");
            Err(e.into())
        }
    }
}

/// Extract repository name from common Git URL forms:
/// - https://host/org/repo.git
/// - ssh://git@host/org/repo.git
/// - git@host:org/repo.git
fn extract_repo_name(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let last = if let Some(i) = trimmed.rfind('/') {
        &trimmed[i + 1..]
    } else if let Some(i) = trimmed.rfind(':') {
        &trimmed[i + 1..]
    } else {
        trimmed
    };
    Some(last.trim_end_matches(".git").to_string())
}

/// Ensure the base directory exists.
fn ensure_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        info!(path = %dir.display(), "created base dir");
    } else {
        fs::remove_dir_all(dir)?;
        fs::create_dir_all(dir)?;
        debug!(path = %dir.display(), "update exists dir");
    }
    Ok(())
}
