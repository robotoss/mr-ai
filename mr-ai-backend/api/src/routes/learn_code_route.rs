use services::git::git_clone_with_token;

pub async fn learn_code() -> &'static str {
    let _ = git_clone_with_token("git@gitlab.com:kulllgar/testprojectmain.git");

    "Hello, World!"
}
