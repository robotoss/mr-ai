//! Per-hypothesis review prompt builder. Sprint 4b of 🅰 LLM Quality.
//!
//! v1 splits the monolithic Smart-tier review into one focused call
//! per hypothesis: one anchor, one file, one short verdict. The
//! response is constrained to a strict JSON schema so the worker can
//! validate it and fall back to a heuristic stub on refusal.

use serde::{Deserialize, Serialize};

use crate::review::prompt::{LlmPlannedAnchor, LlmReviewChangeMeta, LlmReviewTarget};

/// Schema-constrained payload the LLM is asked to return. Caller
/// parses the JSON response into this struct via `serde_json`; any
/// parse failure is classified as `json_invalid`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HypothesisVerdict {
    /// Echo of the hypothesis identifier (`H1`, `H2`, …). Lets us
    /// detect a confused LLM that returns a different hypothesis.
    pub hypothesis_id: String,
    /// One of "supported", "rejected", "uncertain".
    pub verdict: String,
    /// Short reviewer-facing comment (≤ 400 chars by convention).
    pub comment: String,
    /// LLM self-confidence in [0.0, 1.0].
    pub confidence: f32,
}

/// Build a focused prompt for a single hypothesis. Pure string assembly
/// so it's trivially testable without a gateway.
pub fn build_per_hypothesis_prompt(
    change: &LlmReviewChangeMeta,
    target: &LlmReviewTarget,
    anchor: &LlmPlannedAnchor,
) -> String {
    let mut buf = String::with_capacity(2048);
    buf.push_str("ROLE\n");
    buf.push_str(
        "You are a senior code reviewer. Evaluate exactly ONE hypothesis \
         about ONE diff hunk. Stay grounded in the diff; do not invent \
         changes that aren't present.\n\n",
    );

    buf.push_str("CHANGE\n");
    buf.push_str(&format!("project: {}\n", change.project));
    buf.push_str(&format!("mr_iid: {}\n", change.iid));
    buf.push_str(&format!("title: {}\n\n", change.title));

    buf.push_str("HYPOTHESIS\n");
    buf.push_str(&format!("id: {}\n", anchor.hypothesis_id));
    buf.push_str(&format!("priority: {}\n", anchor.priority));
    buf.push_str(&format!("kind: {}\n", anchor.kind));
    buf.push_str(&format!("file: {}\n", target.file_path));
    buf.push_str(&format!(
        "lines: {}-{} (new-side)\n",
        anchor.start_line, anchor.end_line
    ));

    buf.push_str("\nANCHOR_LINES\n");
    for line in &anchor.anchor_lines {
        buf.push_str(line);
        if !line.ends_with('\n') {
            buf.push('\n');
        }
    }

    buf.push_str("\nOUTPUT_JSON_SCHEMA\n");
    buf.push_str(
        r#"{
  "hypothesis_id": "string (must equal HYPOTHESIS.id)",
  "verdict": "supported" | "rejected" | "uncertain",
  "comment": "string (<= 400 chars)",
  "confidence": "number in [0.0, 1.0]"
}"#,
    );

    buf.push_str("\n\nINSTRUCTIONS\n");
    buf.push_str(
        "Return ONLY the JSON object specified above. No markdown, no \
         prose around it. If you cannot evaluate (e.g. context missing), \
         use verdict=\"uncertain\" with confidence <= 0.3.\n",
    );

    buf
}

/// Strip a Markdown code fence around an LLM response. Idempotent —
/// returns the input unchanged if no fence is present. Mirrors the
/// existing helper in `llm_rerank` so we don't have to add a new dep.
pub fn strip_markdown_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    unfenced.trim_end_matches("```").trim()
}

/// Parse and validate the LLM response against `HypothesisVerdict`.
/// Returns `Ok` only when JSON is well-formed AND `hypothesis_id`
/// echoes the expected value (LLM didn't drift to a different
/// hypothesis). Any other case → `Err` so the caller records the
/// outcome as `json_invalid`.
pub fn validate_verdict(
    raw: &str,
    expected_hypothesis_id: &str,
) -> Result<HypothesisVerdict, String> {
    let cleaned = strip_markdown_fences(raw);
    let v: HypothesisVerdict = serde_json::from_str(cleaned)
        .map_err(|e| format!("json_parse: {e}"))?;
    if v.hypothesis_id != expected_hypothesis_id {
        return Err(format!(
            "hypothesis_id_mismatch: expected {expected_hypothesis_id}, got {}",
            v.hypothesis_id
        ));
    }
    if !matches!(v.verdict.as_str(), "supported" | "rejected" | "uncertain") {
        return Err(format!("verdict_unknown: {}", v.verdict));
    }
    if !(0.0..=1.0).contains(&v.confidence) {
        return Err(format!("confidence_out_of_range: {}", v.confidence));
    }
    Ok(v)
}

/// Detect common refusal patterns in the LLM response (e.g. "I cannot
/// help with that", "As an AI"). Used by the worker to classify the
/// outcome as `refused` rather than `json_invalid` when the JSON parse
/// fails because the model returned prose instead.
pub fn looks_like_refusal(raw: &str) -> bool {
    let lower = raw.to_lowercase();
    const NEEDLES: &[&str] = &[
        "i cannot",
        "i can't",
        "as an ai",
        "i'm not able",
        "i am not able",
        "i won't",
        "unable to assist",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(id: &str, priority: &str) -> LlmPlannedAnchor {
        LlmPlannedAnchor {
            hypothesis_id: id.into(),
            priority: priority.into(),
            kind: "PossibleBug".into(),
            start_line: 10,
            end_line: 20,
            anchor_lines: vec!["+ let x = y / 0;".into()],
        }
    }

    fn target(file_path: &str) -> LlmReviewTarget {
        LlmReviewTarget {
            file_path: file_path.into(),
            hunk_index: 0,
            prompt_text: String::new(),
            planned_anchors: Vec::new(),
        }
    }

    fn change() -> LlmReviewChangeMeta {
        LlmReviewChangeMeta {
            provider: "gitlab".into(),
            project: "acme".into(),
            iid: 42,
            title: "fix bug".into(),
            description: String::new(),
            author_name: "alice".into(),
            web_url: "https://example/mr".into(),
            gitlab_head_sha: "h".into(),
            gitlab_base_sha: "b".into(),
            gitlab_start_sha: None,
        }
    }

    #[test]
    fn prompt_includes_hypothesis_priority_and_anchor_lines() {
        let prompt = build_per_hypothesis_prompt(
            &change(),
            &target("src/foo.rs"),
            &anchor("H1", "High"),
        );
        assert!(prompt.contains("HYPOTHESIS"));
        assert!(prompt.contains("id: H1"));
        assert!(prompt.contains("priority: High"));
        assert!(prompt.contains("src/foo.rs"));
        assert!(prompt.contains("let x = y / 0"));
        assert!(prompt.contains("OUTPUT_JSON_SCHEMA"));
    }

    #[test]
    fn validate_verdict_accepts_supported_with_valid_confidence() {
        let raw = r#"{
            "hypothesis_id": "H1",
            "verdict": "supported",
            "comment": "Division by literal zero panics at runtime.",
            "confidence": 0.92
        }"#;
        let v = validate_verdict(raw, "H1").expect("should parse");
        assert_eq!(v.verdict, "supported");
        assert!((v.confidence - 0.92).abs() < 1e-6);
    }

    #[test]
    fn validate_verdict_rejects_mismatched_hypothesis_id() {
        let raw = r#"{
            "hypothesis_id": "H2",
            "verdict": "supported",
            "comment": "x",
            "confidence": 0.5
        }"#;
        let err = validate_verdict(raw, "H1").unwrap_err();
        assert!(err.contains("hypothesis_id_mismatch"), "got: {err}");
    }

    #[test]
    fn validate_verdict_rejects_unknown_verdict() {
        let raw = r#"{
            "hypothesis_id": "H1",
            "verdict": "definitely_yes",
            "comment": "x",
            "confidence": 0.5
        }"#;
        assert!(validate_verdict(raw, "H1").is_err());
    }

    #[test]
    fn strip_markdown_fences_handles_json_fence() {
        let raw = "```json\n{\"a\":1}\n```";
        assert_eq!(strip_markdown_fences(raw), "{\"a\":1}");
    }

    #[test]
    fn looks_like_refusal_detects_common_phrases() {
        assert!(looks_like_refusal("I cannot help with that request."));
        assert!(looks_like_refusal("As an AI language model, I..."));
        assert!(!looks_like_refusal(r#"{"hypothesis_id": "H1"}"#));
    }
}
