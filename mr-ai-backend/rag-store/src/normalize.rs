//! Text normalization utilities to keep embeddings compact and relevant.
//!
//! Provides lightweight transformations for code snippets and metadata fields,
//! making them more suitable for vector embeddings.

use tracing::debug;

/// Normalize code text with minimal layout disruption.
///
/// - Trims trailing whitespace on each line.
/// - Collapses multiple blank lines into a single one.
/// - Preserves code structure (newlines are kept).
/// - Stops once `max_chars` is reached to avoid overlong strings.
///
/// This is intentionally conservative for code so we don't destroy indentation.
pub fn normalize_code_light(s: &str, max_chars: usize) -> String {
    debug!("normalize_code_light: input_len={}", s.len());

    let mut out = String::with_capacity(s.len().min(max_chars));
    let mut blank_run = 0usize;

    for line in s.lines() {
        let line = line.trim_end();

        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue; // skip consecutive blank lines
            }
        } else {
            blank_run = 0;
        }

        // Check if adding this line would exceed max_chars.
        if out.len() + line.len() + 1 > max_chars {
            break;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

/// Compactly joins several short fields into a single paragraph.
///
/// - Joins non-empty parts with `" · "` as a separator.
/// - Stops once `max_chars` is reached.
/// - Limits the number of joined parts (to avoid huge metadata dumps).
///
/// # Example
/// ```
/// let parts = vec!["fn foo()", "bar.rs", "line 10"];
/// let s = join_compact(&parts, 50);
/// assert_eq!(s, "fn foo() · bar.rs · line 10");
/// ```
pub fn join_compact(parts: &[&str], max_chars: usize) -> String {
    const MAX_PARTS: usize = 7; // Safety cap to avoid runaway joins.

    let mut out = String::with_capacity(max_chars.min(128));
    for (i, p) in parts.iter().filter(|p| !p.is_empty()).enumerate() {
        if i >= MAX_PARTS {
            break;
        }

        // Add separator if not the first element.
        if !out.is_empty() {
            out.push_str(" · ");
        }

        if out.len() + p.len() > max_chars {
            break;
        }

        out.push_str(p);
    }

    out
}
