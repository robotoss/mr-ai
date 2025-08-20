//! Public API types re-used by external crates (e.g., the HTTP API layer).

/// Options that control retrieval and prompt building for a single question.
///
/// Setting a field to `0` means: "use the value from env-config".
///
/// # Example
/// ```
/// use contextor::AskOptions;
/// let opts = AskOptions { top_k: 8, context_k: 5 };
/// assert_eq!(opts.top_k, 8);
/// ```
#[derive(Clone, Debug, Default)]
pub struct AskOptions {
    /// Initial top-K candidates to fetch from the vector store.
    /// If `0`, the library falls back to `RAG_TOP_K` from env.
    pub top_k: u64,
    /// Final number of chunks included in the prompt after selection.
    /// If `0`, the library falls back to `CTX_K` from env.
    pub context_k: usize,
}

/// A compact record of a context chunk that was fed to the LLM.
///
/// # Example
/// ```
/// use contextor::UsedChunk;
/// let c = UsedChunk {
///     score: 0.92,
///     source: Some("path/file.dart".into()),
///     fqn: Some("BaseHomePage::build".into()),
///     kind: Some("Method".into()),
///     text: "Widget build(BuildContext ctx) { ... }".into(),
/// };
/// assert!(c.score > 0.0);
/// ```
#[derive(Clone, Debug)]
pub struct UsedChunk {
    pub score: f32,
    pub source: Option<String>,
    pub fqn: Option<String>,
    pub kind: Option<String>,
    pub text: String,
}

/// Final answer together with the exact context passed to the model.
///
/// # Example
/// ```
/// use contextor::{QaAnswer, UsedChunk};
/// let qa = QaAnswer {
///     answer: "It is defined in BaseHomePage".into(),
///     context: vec![UsedChunk {
///         score: 0.9, source: None, fqn: None, kind: None, text: "..." .into()
///     }],
/// };
/// assert!(!qa.answer.is_empty());
/// ```
#[derive(Clone, Debug)]
pub struct QaAnswer {
    pub answer: String,
    pub context: Vec<UsedChunk>,
}
