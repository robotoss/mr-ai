use serde::Deserialize;

#[derive(Deserialize)]
pub struct GitProjectsPayload {
    pub urls: Vec<String>,
}
