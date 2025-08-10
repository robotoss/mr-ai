use std::{
    env, fs,
    path::{Path, PathBuf},
};

use futures::{StreamExt, TryStreamExt};
use git2::{Cred, FetchOptions, RemoteCallbacks};

/// Clone a list of repositories in parallel (default up to `max_concurrency` at the same time).
pub async fn git_clone_list_async(
    repo_urls: Vec<String>,
    max_concurrency: usize,
) -> Result<(), anyhow::Error> {
    let project_name = env::var("PROJECT_NAME").expect("PROJECT_NAME must be set in environment");
    let base_dir = PathBuf::from(format!("code_data/{project_name}"));
    ensure_dir(&base_dir)?;

    // Parallel processing with limited concurrency
    futures::stream::iter(repo_urls.into_iter().map(|url| {
        let base_dir = base_dir.clone();
        async move {
            tokio::task::spawn_blocking(move || git_clone_one(&url, &base_dir))
                .await
                .map_err(|e| anyhow::anyhow!("JoinError: {e}"))
        }
    }))
    .buffer_unordered(max_concurrency.max(1))
    .try_collect::<Vec<_>>() // Fail fast if any repo fails to clone
    .await?;

    Ok(())
}

/// Legacy-compatible single-repo async wrapper.
pub async fn git_clone_with_token_async(repo_url: String) -> Result<(), git2::Error> {
    tokio::task::spawn_blocking(move || {
        let project_name =
            env::var("PROJECT_NAME").expect("PROJECT_NAME must be set in environment");
        let base_dir = PathBuf::from(format!("code_data/{project_name}"));
        ensure_dir(&base_dir).map_err(to_git2_err)?;
        git_clone_one(&repo_url, &base_dir)
    })
    .await
    .unwrap()
}

/// Clone a single repository into `{base_dir}/{repo_name}` using SSH key authentication.
fn git_clone_one(repo_url: &str, base_dir: &Path) -> Result<(), git2::Error> {
    println!("🔗 Repository URL: {repo_url}");

    let repo_name = extract_repo_name(repo_url).unwrap_or_else(|| {
        eprintln!("❌ Failed to extract repository name from URL");
        String::from("unnamed_repo")
    });
    println!("📁 Repository name extracted: {repo_name}");

    let target_dir = base_dir.join(&repo_name);
    println!("📂 Target clone directory: {}", target_dir.display());

    // Remove only this repo's target directory if it exists
    clean_target_dir(&target_dir)?;

    // Configure SSH key credentials
    let key_path = Path::new("ssh_keys/bot_key");
    println!("🔐 Using SSH key: {}", key_path.display());

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, username, _allowed| {
        let user = username.unwrap_or("git");
        Cred::ssh_key(user, None, key_path, None)
    });

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);

    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fetch_options);

    println!("🚀 Starting clone to: {}", target_dir.display());
    match builder.clone(repo_url, &target_dir) {
        Ok(_) => {
            println!("✅ Clone completed successfully: {}", target_dir.display());
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Clone failed: {e}");
            Err(e)
        }
    }
}

/// Extracts the repository name from a variety of Git URL formats:
/// - `https://host/org/repo.git`
/// - `ssh://git@host/org/repo.git`
/// - `git@host:org/repo.git`
fn extract_repo_name(repo_url: &str) -> Option<String> {
    let trimmed = repo_url.trim_end_matches('/');
    let last_segment = if let Some(idx) = trimmed.rfind('/') {
        &trimmed[idx + 1..]
    } else if let Some(idx) = trimmed.rfind(':') {
        // Handle scp-like syntax: git@host:org/repo.git
        &trimmed[idx + 1..]
    } else {
        trimmed
    };
    Some(last_segment.trim_end_matches(".git").to_string())
}

/// Creates the given directory if it does not exist.
fn ensure_dir(dir: &Path) -> Result<(), std::io::Error> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        println!("📦 Created base directory: {}", dir.display());
    }
    Ok(())
}

/// Deletes the target repo directory if it exists.
fn clean_target_dir(target: &Path) -> Result<(), git2::Error> {
    if target.exists() {
        println!("🧹 Removing existing target: {}", target.display());
        fs::remove_dir_all(target).map_err(to_git2_err)?;
    }
    Ok(())
}

/// Converts `std::io::Error` to `git2::Error` for consistent error handling.
fn to_git2_err(e: std::io::Error) -> git2::Error {
    git2::Error::from_str(&format!("io error: {e}"))
}
