use rag_base::structs::search_result::CodeSearchResult;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SearchVectorBaseResponse {
    pub message: String,
    pub query: String,
    pub results: Vec<CodeSearchResult>,
}
