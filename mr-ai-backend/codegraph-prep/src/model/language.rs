//! Language taxonomy and helpers.
//!
//! This module defines a compact enum for supported languages and small
//! utilities for file-extension based detection. We intentionally keep this
//! module free of Tree-sitter grammars to avoid heavy compile-time coupling.
//! Language→grammar mapping should live in language-specific modules.

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

/// Supported programming languages for this library.
///
/// Keep the set tight and add variants deliberately. Unknown languages should be
/// handled by a generic builder outside of this module.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageKind {
    Dart,
    Python,
    JavaScript,
    TypeScript,
    Rust,
}

impl Display for LanguageKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LanguageKind::Dart => "dart",
            LanguageKind::Python => "python",
            LanguageKind::JavaScript => "javascript",
            LanguageKind::TypeScript => "typescript",
            LanguageKind::Rust => "rust",
        })
    }
}

impl LanguageKind {
    /// Best-effort detection by file extension.
    ///
    /// Returns `None` for unsupported extensions; callers may fall back to
    /// generic handling. The mapping is intentionally conservative.
    pub fn from_extension(ext: &str) -> Option<Self> {
        let e = ext.to_ascii_lowercase();
        match e.as_str() {
            "dart" => Some(Self::Dart),
            "py" => Some(Self::Python),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "ts" | "tsx" => Some(Self::TypeScript),
            "rs" => Some(Self::Rust),
            _ => None,
        }
    }
}
