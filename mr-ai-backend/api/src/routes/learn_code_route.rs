use services::git::git_clone_with_token_async;

pub async fn learn_code() -> &'static str {
    let _ = git_clone_with_token_async("git@gitlab.com:kulllgar/testprojectmain.git".to_string())
        .await
        .unwrap();

    "Hello, World!"
}
