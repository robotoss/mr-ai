//! Source location model and helpers.
//!
//! `Span` stores *both* line and byte ranges to support robust slicing and
//! diagnostics. Lines are 1-based (as commonly reported to users), while bytes
//! are 0-based offsets into the original text.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Inclusive start line (1-based).
    pub start_line: usize,
    /// Inclusive end line (1-based).
    pub end_line: usize,
    /// Inclusive start byte (0-based).
    pub start_byte: usize,
    /// Exclusive end byte (0-based).
    pub end_byte: usize,
}

impl Span {
    /// Build a span from line and byte ranges.
    pub fn new(start_line: usize, end_line: usize, start_byte: usize, end_byte: usize) -> Self {
        Self {
            start_line,
            end_line,
            start_byte,
            end_byte,
        }
    }

    /// Lines spanned (1-based inclusive).
    pub fn line_count(&self) -> usize {
        if self.end_line >= self.start_line {
            self.end_line - self.start_line + 1
        } else {
            0
        }
    }

    /// Bytes spanned.
    pub fn byte_len(&self) -> usize {
        if self.end_byte >= self.start_byte {
            self.end_byte - self.start_byte
        } else {
            0
        }
    }

    /// Merge two spans (assuming they belong to the same file).
    pub fn merge(a: &Span, b: &Span) -> Span {
        Span {
            start_line: a.start_line.min(b.start_line),
            end_line: a.end_line.max(b.end_line),
            start_byte: a.start_byte.min(b.start_byte),
            end_byte: a.end_byte.max(b.end_byte),
        }
    }

    /// Extract a snippet from `text` by byte offsets, with *safe* bounds.
    /// This function does not add any extra context.
    pub fn slice_text<'a>(&self, text: &'a str) -> &'a str {
        let len = text.len();
        let start = self.start_byte.min(len);
        let end = self.end_byte.min(len).max(start);
        &text[start..end]
    }

    /// Extract a snippet from `text` and expand it with `context_lines` above/below.
    ///
    /// The expansion is computed line-wise to avoid breaking UTF-8 boundaries.
    /// If `context_lines` is 0, behaves like `slice_text`.
    pub fn slice_with_context(&self, text: &str, context_lines: usize) -> String {
        if context_lines == 0 {
            return self.slice_text(text).to_owned();
        }

        // Build a line index of byte offsets.
        let mut line_starts = Vec::with_capacity(1024);
        line_starts.push(0usize);
        for (idx, b) in text.bytes().enumerate() {
            if b == b'\n' {
                // next line starts at idx+1
                line_starts.push(idx + 1);
            }
        }
        // Add a sentinel past-the-end to simplify slicing computations.
        line_starts.push(text.len());

        // Convert target window into byte offsets.
        let start_line = self.start_line.saturating_sub(1); // 0-based
        let end_line = self.end_line.saturating_sub(1);

        let ctx_start_line = start_line.saturating_sub(context_lines);
        let ctx_end_line = (end_line + context_lines).min(line_starts.len().saturating_sub(2));

        let start_byte = *line_starts.get(ctx_start_line).unwrap_or(&0);
        let end_byte = *line_starts.get(ctx_end_line + 1).unwrap_or(&text.len());

        text[start_byte..end_byte].to_owned()
    }
}
