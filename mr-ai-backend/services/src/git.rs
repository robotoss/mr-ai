use std::{env, fs, path::Path};

use git2::{Cred, FetchOptions, RemoteCallbacks};

pub async fn git_clone_with_token_async(repo_url: String) -> Result<(), git2::Error> {
    tokio::task::spawn_blocking(move || git_clone_with_token(&repo_url))
        .await
        .unwrap()
}

fn git_clone_with_token(repo_url: &str) -> Result<(), git2::Error> {
    let project_name = env::var("PROJECT_NAME").expect("PROJECT_NAME must be set in environment");

    println!("🔗 Repository URL: {}", repo_url);

    let git_name = extract_repo_name(repo_url).unwrap_or_else(|| {
        eprintln!("❌ Failed to extract repository name from URL");
        String::from("unnamed_repo")
    });

    println!("📁 Repository name extracted: {}", git_name);

    clean_project_dir(&project_name);

    let target_dir = format!("code_data/{}/{}", project_name, git_name);
    println!("📂 Target clone directory: {}", target_dir);

    let token_path = "ssh_keys/bot_key";
    println!("🔐 Reading token from: {}", token_path);

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, username, _allowed| {
        let user = username.unwrap_or("git");
        Cred::ssh_key(user, None, std::path::Path::new("ssh_keys/bot_key"), None)
    });

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);

    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fetch_options);

    println!("🚀 Starting clone to: {}", target_dir);
    match builder.clone(repo_url, std::path::Path::new(&target_dir)) {
        Ok(_) => {
            println!("✅ Clone completed successfully: {}", target_dir);
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Clone failed: {}", e);
            Err(e)
        }
    }
}

fn extract_repo_name(repo_url: &str) -> Option<String> {
    let last = repo_url.split('/').last()?.trim_end_matches(".git");
    Some(last.to_string())
}

fn clean_project_dir(project_name: &str) {
    let base_path = format!("code_data/{}", project_name);
    let base_path = Path::new(&base_path);

    if base_path.exists() && base_path.is_dir() {
        println!("🧹 Cleaning directory: {}", base_path.display());

        for entry in fs::read_dir(base_path).expect("Failed to read project directory") {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_dir() {
                    fs::remove_dir_all(&path).expect("Failed to remove subdirectory");
                } else {
                    fs::remove_file(&path).expect("Failed to remove file");
                }
            }
        }
    } else {
        println!(
            "📁 Directory does not exist or is not a folder: {}",
            base_path.display()
        );
    }
}
