//! Language hook for the upstream tree-sitter-rust grammar.

use tree_sitter::Language;

/// Return the Rust language for tree-sitter.
#[inline]
pub fn language() -> Language {
    let lang: Language = tree_sitter_rust::LANGUAGE.into();
    lang
}
