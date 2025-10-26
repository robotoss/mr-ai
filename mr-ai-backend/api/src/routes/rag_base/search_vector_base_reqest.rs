use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct SearchVectorBaseRequest {
    pub query: String,
    pub k: Option<usize>,
}
