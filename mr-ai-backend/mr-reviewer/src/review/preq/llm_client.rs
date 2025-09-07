//! Thin adapter to call the existing LlmRouter with a small/FAST model
//! and sanitize the JSON output.

use crate::errors::MrResult;
use crate::review::llm::LlmRouter;

/// Call the FAST model to ask: "what context do you need?".
pub async fn ask_need_context(router: &LlmRouter, prompt: &str) -> MrResult<String> {
    // Use FAST to keep latency/cost low; refine quality via RAG later.
    let raw = router.generate_fast(prompt).await?;
    Ok(raw)
}

/// Remove any markdown fences and pre/post-text; extract the first JSON object.
/// This is deliberately tolerant: we accept `{...}` anywhere in the string.
pub fn sanitize_json_block(s: &str) -> String {
    // Remove code fences if present
    let no_fence = s
        .replace("```json", "")
        .replace("```", "")
        .replace('\u{feff}', "") // BOM
        .trim()
        .to_string();

    // Try to find the first '{' and the matching last '}'.
    if let (Some(start), Some(end)) = (no_fence.find('{'), no_fence.rfind('}')) {
        let candidate = &no_fence[start..=end];
        // Quick and dirty check: is this plausible JSON?
        if candidate.contains(":") {
            return candidate.to_string();
        }
    }
    // Fallback: return as-is; caller will attempt JSON parse (and log on failure).
    no_fence
}
