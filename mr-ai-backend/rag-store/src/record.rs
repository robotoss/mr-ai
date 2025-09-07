//! Core data models used by the library.

use core::fmt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Canonical record stored in Qdrant and used in ingestion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RagRecord {
    pub id: String,
    pub text: String,
    pub source: Option<String>,
    pub embedding: Option<Vec<f32>>,
    #[serde(default)]
    pub extra: BTreeMap<String, Value>,
}

/// Query parameters for RAG retrieval.
pub struct RagQuery<'a> {
    pub text: &'a str,
    pub top_k: u64,
    pub filter: Option<RagFilter>,
}

/// A single retrieval hit returned from Qdrant.
///
/// Contains both ranking score and canonical metadata fields.
/// These fields are designed to provide just enough context for
/// code review / RAG augmentation without overloading the model.
#[derive(Clone, Debug)]
pub struct RagHit {
    /// Relevance score from vector search.
    pub score: f32,

    /// Canonical text used for embeddings (usually signature+doc).
    pub text: String,

    /// Original code snippet for display / context building.
    pub snippet: Option<String>,

    /// Path to source file.
    pub source: Option<String>,

    /// Programming language (e.g. "dart", "rust").
    pub language: Option<String>,

    /// Entity kind (e.g. "Class", "Function", "File").
    pub kind: Option<String>,

    /// Fully qualified name (e.g. `MyClass::myMethod`).
    pub fqn: Option<String>,

    /// Free-form tags (e.g. `["test","widget","api"]`).
    pub tags: Vec<String>,

    /// Lightweight graph neighbors (used for graph-RAG expansion).
    pub neighbors: Vec<serde_json::Value>,

    /// Auxiliary metrics (e.g. lines of code, params count).
    pub metrics: Option<serde_json::Value>,

    /// Raw payload (for debugging or extra fields).
    pub raw_payload: serde_json::Value,
}

/// Pretty printing for `RagHit` to keep logs readable.
impl fmt::Display for RagHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "─ Hit (score={:.3})", self.score)?;
        if let (Some(k), Some(lang)) = (&self.kind, &self.language) {
            writeln!(f, "  kind    : {} ({})", k, lang)?;
        } else if let Some(k) = &self.kind {
            writeln!(f, "  kind    : {}", k)?;
        } else if let Some(lang) = &self.language {
            writeln!(f, "  language: {}", lang)?;
        }
        if let Some(fqn) = &self.fqn {
            if !fqn.is_empty() {
                writeln!(f, "  fqn     : {}", fqn)?;
            }
        }
        if let Some(src) = &self.source {
            writeln!(f, "  source  : {}", src)?;
        }
        writeln!(f, "  text    : {}", self.text)?;

        if let Some(snippet) = &self.snippet {
            let shown = clamp_snippet(snippet, 800, 100);
            if !shown.is_empty() {
                writeln!(f, "  snippet :\n    {}", shown.replace('\n', "\n    "))?;
            }
        }

        if !self.tags.is_empty() {
            writeln!(f, "  tags    : {:?}", self.tags)?;
        }
        if let Some(metrics) = &self.metrics {
            writeln!(f, "  metrics : {}", metrics)?;
        }
        if !self.neighbors.is_empty() {
            writeln!(f, "  neighbors: {} entries", self.neighbors.len())?;
        }
        Ok(())
    }
}

/// Returns a trimmed version of code snippet for UI/log/LLM context.
/// Limits both characters and lines to keep context compact.
pub fn clamp_snippet(s: &str, max_chars: usize, max_lines: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars));
    for (i, line) in s.lines().enumerate() {
        if i >= max_lines || out.len() + line.len() + 1 > max_chars {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.len() > max_chars {
        out.truncate(max_chars);
    }
    out.trim_end().to_string()
}

/// Simple filter (placeholder). Extend as needed.
#[derive(Clone, Debug)]
pub struct RagFilter {
    /// Exact match on a field, e.g. {"source": "path/to/file.rs"}
    pub equals: Vec<(String, serde_json::Value)>,
}
