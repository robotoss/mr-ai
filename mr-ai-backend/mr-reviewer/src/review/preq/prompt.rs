//! Prompt builder for "what context do you need?" question.
//! Keep it short and surgical to minimize cost and avoid hallucinations.

use std::fmt::Write;

/// Build a compact, unambiguous prompt that forces STRICT JSON with four keys:
/// `queries`, `need_paths_like`, `need_symbols_like`, `reason`.
///
/// ## Contract enforced on the model
/// - **Return exactly one JSON object on a single line** (no markdown fences, no prose).
/// - **Include all four keys**; never omit any key.
/// - `queries`, `need_paths_like`, `need_symbols_like`: arrays of strings, length ≤ 3. Use `[]` when empty.
/// - `reason`: non-empty string (never null/omitted). If nothing is needed: `"No extra context needed"`.
/// - **No "thinking", no comments, no trailing text**.
///
/// This reduces ambiguity and prevents the model from dropping fields, which previously
/// led to empty blocks in the cleaner.
pub fn build_need_context_prompt(
    language_hint: Option<&str>,
    target_path: Option<&str>,
    allowed_anchors: &[(usize, usize)],
    local_window_numbered: &str,
) -> String {
    let mut s = String::with_capacity(2048);

    let lang = language_hint.unwrap_or("code");
    let path = target_path.unwrap_or("");

    // Role / instruction
    writeln!(
        s,
        "You are a senior {lang} code reviewer (Dart/Flutter experts: assume Dart 3.x). \
Your task is to determine what ADDITIONAL read-only context is needed from a codebase RAG \
to review the diff precisely."
    )
    .ok();

    // Hard, unambiguous output contract
    writeln!(
        s,
        "\nOUTPUT FORMAT (MANDATORY): Return EXACTLY ONE JSON object on a SINGLE LINE with \
EXACTLY these four keys and no extra text before or after:"
    )
    .ok();
    // Show the target shape clearly
    writeln!(
        s,
        r#"{{"queries":["..."],"need_paths_like":["..."],"need_symbols_like":["..."],"reason":"..."}}"#
    ).ok();

    // Field rules
    writeln!(s, "\nRules:").ok();
    writeln!(s, "- Always include all four keys; never omit any key.").ok();
    writeln!(s, "- `queries`, `need_paths_like`, `need_symbols_like`: arrays of strings, length <= 3. Use [] when empty.").ok();
    writeln!(s, "- `reason`: REQUIRED non-empty string. If no extra context is needed, use \"No extra context needed\".").ok();
    writeln!(
        s,
        "- Do NOT output markdown, code fences, comments, or any 'thinking' text."
    )
    .ok();

    if !path.is_empty() {
        writeln!(s, "\nTarget path: {path}").ok();
    }

    // Provide anchors
    if !allowed_anchors.is_empty() {
        let mut ranges = String::new();
        for (i, (a, b)) in allowed_anchors.iter().enumerate() {
            if i > 0 {
                ranges.push_str(", ");
            }
            let _ = write!(&mut ranges, "{a}-{b}");
        }
        writeln!(s, "Allowed anchors (inclusive lines): {ranges}").ok();
    }

    // Provide the local window
    writeln!(
        s,
        "\nLocal window (numbered):\n---\n{local_window_numbered}\n---"
    )
    .ok();

    // Guidance about *what* to ask for (concise, surgical)
    writeln!(
        s,
        "\
Selection guidance:
- Ask only for context that materially affects a correct review of the DIFF.
- Prefer specific router/config/symbols over broad full-file dumps.
- If nothing else is needed, return empty arrays and a short `reason`."
    )
    .ok();

    s
}
