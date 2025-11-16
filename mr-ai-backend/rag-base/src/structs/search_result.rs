use serde::{Deserialize, Serialize};

/// This struct is intended to be returned from the public search API and
/// serialized to JSON for HTTP responses or logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeSearchResult {
    /// Combined similarity score for this stitched block.
    pub score: f32,

    /// Path to the source file.
    pub file: String,

    /// Normalized language name, e.g. "dart", "kotlin".
    pub language: String,

    /// Normalized symbol kind, e.g. "class", "method", "function".
    pub kind: String,

    /// Symbol path in the form "<file>::Class::method".
    pub symbol_path: String,

    /// Short symbol name, e.g. "AppRouting".
    pub symbol: String,

    /// Short signature suitable for preview.
    pub signature: Option<String>,

    /// Short preview snippet used for list views.
    pub snippet: Option<String>,

    /// Stitched source code block from the original file.
    pub code: String,

    /// Zero-based start line (inclusive) of the stitched block.
    pub start_row: u32,

    /// Zero-based end line (exclusive) of the stitched block.
    pub end_row: u32,
}
