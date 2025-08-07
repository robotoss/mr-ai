use git2::{Cred, FetchOptions, RemoteCallbacks};

pub fn git_clone_with_token(repo_url: &str) -> Result<(), git2::Error> {
    println!("🔗 Repository URL: {}", repo_url);

    let git_name = extract_repo_name(repo_url).unwrap_or_else(|| {
        eprintln!("❌ Failed to extract repository name from URL");
        String::from("unnamed_repo")
    });

    println!("📁 Repository name extracted: {}", git_name);

    let target_dir = format!("code_data/{}", git_name);
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
