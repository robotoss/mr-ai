use axum::Json;
use services::git::git_clone_list_async;

use crate::models::git_projects_data::GitProjectsPayload;

pub async fn upload_project_data(Json(payload): Json<GitProjectsPayload>) -> &'static str {
    println!("Get urls: {:?}", payload.urls);

    let _ = git_clone_list_async(payload.urls, 4).await.unwrap();

    "Code Success upload"
}
