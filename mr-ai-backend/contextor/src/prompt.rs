//! Prompt builder: short system message + compact context block.

use rag_store::RagHit;

/// Default system instructions for code-aware answers.
///
/// Keep this short: it consistently improves steering without wasting tokens.
pub const DEFAULT_SYSTEM: &str = r#"
You are a precise code assistant. Be concise, cite files and symbol names when relevant.
Use the provided context as ground truth; if it is insufficient, say so and propose next steps.
"#;

/// Build final user prompt with a labeled context section and char budget.
///
/// The function compacts the context into at most `max_chars`, preserving
/// the ranking order. For each hit, it shows a header with FQN and source,
/// then includes `snippet` if available, otherwise `text`.
///
/// # Example
/// ```
/// # use rag_store::RagHit;
/// # use contextor::prompt::build_user_prompt;
/// let hits: Vec<RagHit> = vec![];
/// let prompt = build_user_prompt("How to X?", &hits, 2000);
/// assert!(prompt.contains("Question:"));
/// ```
pub fn build_user_prompt(question: &str, hits: &[RagHit], max_chars: usize) -> String {
    let mut out = String::new();
    out.push_str("Question:\n");
    out.push_str(question.trim());
    out.push_str("\n\n");

    if !hits.is_empty() {
        out.push_str("Context (top-ranked):\n");
        let mut budget = max_chars;

        for (i, h) in hits.iter().enumerate() {
            let header = format!(
                "==[{}]== {} :: {} (score {:.3})\n",
                i + 1,
                h.fqn.as_deref().unwrap_or(""),
                h.source.as_deref().unwrap_or(""),
                h.score
            );
            let text = h
                .snippet
                .as_deref()
                .unwrap_or_else(|| h.text.as_str())
                .trim();

            // stop if we exceed budget
            if header.len() >= budget {
                break;
            }
            out.push_str(&header);
            budget -= header.len();

            let take = budget.saturating_sub(2);
            if text.len() > take {
                out.push_str(safe_truncate(text, take));
                out.push_str("\n…\n");
                break;
            } else {
                out.push_str(text);
                out.push('\n');
                budget -= text.len() + 1;
            }
        }
        out.push('\n');
        out.push_str("Answer using only the context above when possible.\n");
    }

    out
}

fn safe_truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}
