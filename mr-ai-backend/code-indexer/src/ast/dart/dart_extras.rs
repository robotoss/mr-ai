// ast/dart/dart_extras.rs
//! Dart-specific extras attached to `CodeChunk.extras` as JSON.
//!
//! Keep this narrow and explainability-oriented.

use serde::{Deserialize, Serialize};

/// Per-chunk Dart extras to be serialized into `CodeChunk.extras`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DartChunkExtras {
    /// True if the class behaves like a Flutter widget (heuristic).
    pub is_widget: Option<bool>,
    /// Extracted GoRouter routes (e.g., "/games", "/b").
    pub routes: Vec<String>,
    /// Any additional flags (e.g., "freezed", "riverpod", etc.).
    pub flags: Vec<String>,
}
