//! Safe query runner for Dart extraction.
//!
//! We execute *one* small Tree-sitter pattern at a time. If a pattern
//! doesn't compile for the active grammar version, we simply skip it.
//! This isolates grammar drift and prevents single-pattern failures
//! from breaking the entire extraction.

use tree_sitter::{Language, Node, Query, QueryCursor, QueryMatch, StreamingIterator};

/// Run a single Tree-sitter query pattern if it compiles; otherwise no-op.
///
/// NOTE:
/// - `QueryCursor::matches` is a `StreamingIterator`, so `.next()` yields `&QueryMatch`.
/// - The callback therefore takes `&Query` and `&QueryMatch` by reference.
///
/// # Parameters
/// * `lang` - The active language.
/// * `root` - Root node for the search (usually the file's root).
/// * `code` - Source text used to decode captures.
/// * `pattern` - Tree-sitter pattern; if compilation fails we silently skip.
/// * `on_match` - Callback invoked for each `QueryMatch`.
pub fn run_query_if_supported<F>(
    lang: &Language,
    root: Node,
    code: &str,
    pattern: &str,
    mut on_match: F,
) where
    // HRTB: allow any lifetimes for the borrowed QueryMatch coming from the streaming iterator
    for<'a, 'b> F: FnMut(&Query, &QueryMatch<'a, 'b>),
{
    if let Ok(query) = Query::new(lang, pattern) {
        let mut qc = QueryCursor::new();
        let mut it = qc.matches(&query, root, code.as_bytes());
        while let Some(m) = it.next() {
            on_match(&query, m);
        }
    }
}
