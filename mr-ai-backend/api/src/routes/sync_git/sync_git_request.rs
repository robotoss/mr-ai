use serde::Deserialize;

#[derive(Deserialize)]
pub struct GitProjectsRequest {
    pub urls: Vec<String>,
}
