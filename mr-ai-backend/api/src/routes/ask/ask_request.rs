use serde::{Deserialize, Serialize};

/// Request payload for /ask_question.
#[derive(Debug, Deserialize)]
pub struct AskRequest {
    /// Natural language question.
    pub question: String,
    /// Optional override: number of initial candidates from the vector store.
    #[serde(default)]
    pub top_k: Option<u64>,
    /// Optional override: number of candidates to include in the final prompt.
    #[serde(default)]
    pub context_k: Option<usize>,
}

/// Response payload for /ask_question.
#[derive(Debug, Serialize)]
pub struct AskResponse {
    /// Final model answer (plain text).
    pub answer: String,
    /// Minimal transparency on what context was used.
    pub context: Vec<CtxItem>,
}

/// Small context snippet descriptor.
#[derive(Debug, Serialize)]
pub struct CtxItem {
    pub score: f32,
    pub source: Option<String>,
    pub fqn: Option<String>,
    pub kind: Option<String>,
    /// Short preview of the chunk that was given to the model.
    pub preview: String,
}
