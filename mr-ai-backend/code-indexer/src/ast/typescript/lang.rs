//! Language hooks for tree-sitter-typescript and tree-sitter-tsx.

use std::path::Path;

use tree_sitter::Language;

#[inline]
pub fn language_typescript() -> Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

#[inline]
pub fn language_tsx() -> Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

/// Pick the parser variant by extension. `.tsx` (or `.jsx` treated as
/// best-effort) uses the TSX grammar; everything else gets the plain
/// TypeScript grammar.
pub fn language_for_path(path: &Path) -> Language {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "tsx" || ext == "jsx" {
        language_tsx()
    } else {
        language_typescript()
    }
}
