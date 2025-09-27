//! Language hook for the orchard Dart grammar.

use tree_sitter::Language;

/// Return the Dart language for tree-sitter from the orchard grammar crate.
/// The orchard crate exposes `LANGUAGE` convertible into `tree_sitter::Language`.
#[inline]
pub fn language() -> Language {
    let lang: Language = tree_sitter_dart_orchard::LANGUAGE.into();
    lang
}
