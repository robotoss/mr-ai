//! Prompt builders (FAST + optional SLOW refine), with rule-pack injection.
//!
//! The prompts are **language-agnostic** and include:
//! - Numbered primary snippet (HEAD) with absolute line numbers,
//! - Optional related RAG context (read-only, BASE/external),
//! - Optional full-file content (read-only) to verify global claims (imports/symbols),
//! - **Review policy** assembled from Markdown files in `rules/`,
//! - **CodeFacts**: enclosing FULL snippet + a single CHUNK snippet with {index/total}.
//!
//! Grounding & precedence constraints:
//! - PRIMARY and FULL FILE represent **HEAD** (authoritative).
//! - RELATED is **BASE/external** (non-authoritative).
//! - On conflicts, trust **HEAD**.
//!
//! Output format is strict for reliable downstream parsing.

use std::fs;
use std::path::{Path, PathBuf};

use super::context::PrimaryCtx;
use super::context::types::CodeFacts;
use crate::map::MappedTarget;
use crate::review::RelatedBlock;
use crate::review::context::types::STRICT_OUTPUT_SPEC;

/// Build a strict prompt for the FAST model (single-pass).
///
/// The prompt enforces:
/// - Grounding in HEAD (PRIMARY/FULL FILE),
/// - RELATED as read-only extra context (BASE/external),
/// - Deterministic, machine-parseable output format,
/// - Display of CodeFacts with enclosing + one chunk {index/total}.
pub fn build_strict_prompt(
    tgt: &MappedTarget,
    ctx: &PrimaryCtx,
    related: &[RelatedBlock],
) -> String {
    let mut s = String::new();

    // Role & guardrails
    s.push_str("You are a senior code reviewer.\n");
    s.push_str("Only comment on the DIFFED region. Be specific and concise.\n");
    s.push_str(
        "You MUST consult read-only blocks before any global claim (imports/symbols/cross-file invariants).\n",
    );
    s.push_str("PRIMARY and FULL FILE are HEAD (authoritative). RELATED is BASE/external (non-authoritative).\n");
    s.push_str("On conflicts, trust HEAD.\n");
    // New: strict precedence & speculation guard
    s.push_str("HEAD is the sole source of truth for behavior. RELATED may be used only to validate imports/symbols/types/signatures — never to assert behavior or business logic by itself.\n");
    s.push_str("If a concern depends on RELATED or external code and is not provable from HEAD, use QUESTION mode anchored to the changed lines; otherwise return NO_ISSUES.\n");
    s.push_str("Avoid speculation and hedging. Do not use words like 'Potential', 'Maybe', 'Could', 'Might' in findings. Be categorical or return NO_ISSUES.\n");
    s.push_str("CodeFacts provide: FULL enclosing snippet and a single CHUNK {index/total}.\n\n");

    // Review policy (rules/)
    let path_for_rules = target_path_for_rules(tgt);
    let rules = compose_rules_for_file(path_for_rules);
    if !rules.trim().is_empty() {
        s.push_str("### Review policy\n");
        s.push_str(&rules);
        s.push_str("\n\n");
    }

    // Helper to avoid accidental code-fence termination inside model-rendered text.
    fn sanitize_fence(x: &str) -> String {
        x.replace("```", "``\u{200B}`")
    }

    // PRIMARY (HEAD, numbered)
    s.push_str("PRIMARY (numbered HEAD lines):\n```code\n");
    s.push_str(&sanitize_fence(&ctx.numbered_snippet));
    s.push_str("```\n");

    // CODE FACTS (enclosing + one chunk)
    if let Some(cf) = &ctx.code_facts {
        s.push_str("\nCODE FACTS (read-only):\n```text\n");
        s.push_str(&sanitize_fence(&render_code_facts(cf)));
        s.push_str("\n```\n");
    }

    // RELATED (BASE/external; optional)
    if !related.is_empty() {
        s.push_str("\nRELATED (read-only; BASE/external; non-authoritative — use ONLY to validate imports/symbols/types; do NOT assert behavior based on it):\n```code\n");
        s.push_str(&sanitize_fence(&format_related_for_log(related)));
        s.push_str("\n```\n");
    }

    // FULL FILE (HEAD; optional)
    if let Some(full) = &ctx.full_file_readonly {
        s.push_str(
            "\nFULL FILE (HEAD; read-only; use ONLY to verify imports/symbol presence or cross-line invariants):\n```code\n",
        );
        s.push_str(&sanitize_fence(full));
        s.push_str("\n```\n");
    }

    // Allowed anchors
    s.push_str("\nALLOWED_ANCHORS (inclusive line ranges in the same file):\n");
    for a in &ctx.allowed_anchors {
        s.push_str(&format!("- {}-{}\n", a.start, a.end));
    }

    // Choose output mode:
    // - Default: STRICT block format (human-readable with PATCH blocks)
    // - JSON: inject STRICT_OUTPUT_SPEC and require a pure-JSON body between markers
    let json_mode = std::env::var("MR_REVIEWER_OUTPUT_MODE")
        .map(|v| v.trim().eq_ignore_ascii_case("JSON"))
        .unwrap_or(false);

    if json_mode {
        s.push_str(
            r#"
    <<<BEGIN_STRICT>>>
    ### Output format (STRICT JSON)
    Return ONLY a single JSON object (no prose, no code fences) that follows:
    ```text
    "#,
        );
        s.push_str(STRICT_OUTPUT_SPEC);
        s.push_str(
                r#"
    ```
    Hard constraints:
    - Output ONLY the JSON object. If there are no valid issues, return exactly: { "NoIssues": true }
    <<<END_STRICT>>>
    "#,
            );
        s
    } else {
        // Strict block format (default)
        s.push_str(
                r#"
<<<BEGIN_STRICT>>>
### Output format (STRICT)
For each valid issue return one block:

ANCHOR: <start>-<end>
SEVERITY: High|Medium|Low
TITLE: <short title>
BODY: <concise rationale; reference code/symbols clearly>
PATCH:
```diff
<minimal applicable patch for the anchored lines; NO file headers>
```

QUESTION mode (when key context is missing or uncertainty is high):

Additional QUESTION rules:
- Use QUESTION mode only when a fix is blocked by missing artifacts that are not present in PRIMARY/FULL. Anchor your questions to the changed line(s).
- If your concern relies on RELATED or external code and is not provable from HEAD, prefer a QUESTION. If still uncertain → return NO_ISSUES.

Examples:
ANCHOR: 12-12
SEVERITY: Low
TITLE: NEEDS CONTEXT: import usage
BODY: Evidence in PRIMARY shows `import x`, but no clear symbol usage in the snippet.
Questions:

1. Which symbol from `x` is expected to be used here? (Why: avoid false "unused import"; Need: usage example or reference line)

2. Is there a side-effect import expected? (Why: tree-shaking exceptions; Need: build/config hint)


Rules:

Anchor coupling & precedence:
- Every comment MUST directly pertain to code within the specified ANCHOR range. Do not discuss other screens/modules unless reflected in these lines.
- HEAD is the only source for behavior claims. RELATED may NOT initiate a finding without confirmation in HEAD. Use RELATED only to validate imports/symbols/types/signatures.
- Hedging language is not allowed ("Potential", "Maybe", "Could", "Might"). Be definitive or return NO_ISSUES.

Hard constraints:
    <<<END_STRICT>>>
    "#,
    );

        s
    }
}

/// Build a refine prompt for the SLOW model; keeps the same strict format.
///
/// The refine stage receives the previous block and must:
/// - Improve precision and justification,
/// - Keep anchors valid,
/// - Remove speculation,
/// - Preserve the STRICT output format.
pub fn build_refine_prompt(
    maybe_prev: Option<&crate::review::policy::ParsedFinding>,
    tgt: &MappedTarget,
    ctx: &PrimaryCtx,
    related: &[RelatedBlock],
) -> String {
    let mut s = String::new();
    s.push_str("Refine the draft below while preserving the STRICT format.\n");
    s.push_str("Increase precision, keep anchors valid, and remove any speculation.\n");
    s.push_str("Do not weaken guardrails: if the draft relies on RELATED or is not provable from HEAD for the given ANCHOR, either convert to QUESTION (anchored) or return NO_ISSUES.\n\n");

    if let Some(prev) = maybe_prev {
        s.push_str("PREVIOUS DRAFT:\n```\n");
        s.push_str(&prev.raw_block);
        s.push_str("\n```\n\n");
    }

    s.push_str(&build_strict_prompt(tgt, ctx, related));
    s
}

/// Render `CodeFacts` into a compact, deterministic text block for the prompt.
///
/// The block explicitly includes:
/// - file and anchor,
/// - optional enclosing descriptor,
/// - chunk meta `{index/total}` and its line bounds,
/// - lightweight signals (calls, writes, control_flow, cleanup_like),
/// - FULL enclosing snippet,
/// - one CHUNK snippet.
fn render_code_facts(cf: &CodeFacts) -> String {
    fn list(v: &[String]) -> String {
        if v.is_empty() {
            "[]".into()
        } else {
            format!("[{}]", v.join(", "))
        }
    }

    let mut out = String::new();
    out.push_str(&format!("file: {}\n", cf.file));
    out.push_str(&format!("anchor: {}..{}\n", cf.anchor.start, cf.anchor.end));
    if let Some(enc) = &cf.enclosing {
        out.push_str(&format!(
            "enclosing: {} {} [{}..{}]\n",
            enc.kind, enc.name, enc.start_line, enc.end_line
        ));
    } else {
        out.push_str("enclosing: <none>\n");
    }
    out.push_str(&format!(
        "chunk: {}/{} lines {}..{}\n",
        cf.chunk.index, cf.chunk.total, cf.chunk.from, cf.chunk.to
    ));
    out.push_str(&format!("calls_top: {}\n", list(&cf.calls_top)));
    out.push_str(&format!("writes: {}\n", list(&cf.writes)));
    out.push_str(&format!("control_flow: {}\n", list(&cf.control_flow)));
    out.push_str(&format!("cleanup_like: {}\n", list(&cf.cleanup_like)));
    out.push_str("--- ENCLOSING ---\n");
    out.push_str(&cf.enclosing_snippet);
    out.push('\n');
    out.push_str(&format!(
        "--- CHUNK ({}/{}) ---\n",
        cf.chunk.index, cf.chunk.total
    ));
    out.push_str(&cf.chunk.snippet);
    out
}

// -------- rule-pack loader (no language filters, just prompt guidance) --------

fn rules_root() -> PathBuf {
    std::env::var("MR_REVIEWER_RULES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("rules"))
}

fn target_path_for_rules(tgt: &MappedTarget) -> &str {
    match &tgt.target {
        crate::map::TargetRef::Line { path, .. }
        | crate::map::TargetRef::Range { path, .. }
        | crate::map::TargetRef::Symbol { path, .. }
        | crate::map::TargetRef::File { path } => path.as_str(),
        crate::map::TargetRef::Global => "",
    }
}

fn detect_lang_folder(path: &str) -> &'static str {
    let p = path.to_ascii_lowercase();
    if p.ends_with(".dart") {
        "dart"
    } else if p.ends_with(".rs") {
        "rust"
    } else if p.ends_with(".ts") {
        "ts"
    } else if p.ends_with(".tsx") || p.ends_with(".jsx") || p.ends_with(".js") {
        "js"
    } else if p.ends_with(".py") {
        "python"
    } else if p.ends_with(".java") {
        "java"
    } else if p.ends_with(".kt") || p.ends_with(".kts") {
        "kotlin"
    } else if p.ends_with(".cc")
        || p.ends_with(".cpp")
        || p.ends_with(".cxx")
        || p.ends_with(".hpp")
    {
        "cpp"
    } else if p.ends_with(".cs") {
        "csharp"
    } else if p.ends_with(".go") {
        "go"
    } else if p.ends_with(".php") {
        "php"
    } else {
        "other"
    }
}

/// Compose review rules for a given file path.
///
/// - Reads from `MR_REVIEWER_RULES_DIR/global/*.md`
/// - Reads from language-specific bucket (e.g. `rules/dart/*.md`)
/// - Concatenates results with separators
/// - Returns empty string if nothing found
pub fn compose_rules_for_file(path: &str) -> String {
    let root = rules_root();
    let mut chunks = Vec::new();

    // Global rules
    let global_dir = root.join("global");
    match read_dir_concat(&global_dir) {
        Some(g) => {
            tracing::info!(
                "rules: loaded global rules from {} ({} chars)",
                global_dir.display(),
                g.len()
            );
            chunks.push(format!("## Global rules\n\n{}", g));
        }
        None => {
            tracing::debug!("rules: no global rules found in {}", global_dir.display());
        }
    }

    // Language-specific rules
    if !path.is_empty() {
        let lang_folder = detect_lang_folder(path);
        let lang_dir = root.join(&lang_folder);
        match read_dir_concat(&lang_dir) {
            Some(l) => {
                tracing::info!(
                    "rules: loaded language rules for '{}' from {} ({} chars)",
                    lang_folder,
                    lang_dir.display(),
                    l.len()
                );
                chunks.push(format!("## Language rules ({})\n\n{}", lang_folder, l));
            }
            None => {
                tracing::debug!(
                    "rules: no language rules found for '{}' in {}",
                    lang_folder,
                    lang_dir.display()
                );
            }
        }
    }

    if chunks.is_empty() {
        tracing::warn!(
            "rules: no applicable rules found for file {} (root={})",
            path,
            root.display()
        );
        String::new()
    } else {
        chunks.join("\n\n---\n\n")
    }
}

fn read_dir_concat(dir: &Path) -> Option<String> {
    if !dir.exists() {
        return None;
    }
    let mut files = match fs::read_dir(dir) {
        Ok(r) => r
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
            .collect::<Vec<_>>(),
        Err(_) => return None,
    };
    files.sort();

    let mut buf = String::new();
    for p in files {
        if let Ok(s) = fs::read_to_string(&p) {
            buf.push_str(&format!(
                "### {}\n\n{}\n\n",
                p.file_name().unwrap().to_string_lossy(),
                s
            ));
        }
    }
    if buf.trim().is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// Helper: pretty dump RELATED blocks into one debug string (for logs/telemetry).
pub fn format_related_for_log(blocks: &[RelatedBlock]) -> String {
    let mut s = String::new();
    for (i, b) in blocks.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = writeln!(
            s,
            "-- RELATED[{i}] path={} lang={} why={}",
            b.path,
            if b.language.is_empty() {
                "-"
            } else {
                &b.language
            },
            b.why.as_deref().unwrap_or("-")
        );
        let _ = writeln!(s, "{}", b.snippet);
        let _ = writeln!(s, "-----");
    }
    s
}
