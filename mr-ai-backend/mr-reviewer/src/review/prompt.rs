//! Prompt builders (FAST + optional SLOW refine), with rule-pack injection.
//!
//! The prompts are **language-agnostic** and include:
//! - Numbered primary snippet (HEAD) with absolute line numbers,
//! - Optional related RAG context (read-only),
//! - Optional full-file content (read-only) to verify global claims (imports/symbols),
//! - **Review policy** assembled from Markdown files in `rules/`.
//!
//! Output format is strict for reliable downstream parsing.

use std::fs;
use std::path::{Path, PathBuf};

use super::context::PrimaryCtx;
use crate::map::MappedTarget;

/// Build strict prompt for the FAST model.
pub fn build_strict_prompt(tgt: &MappedTarget, ctx: &PrimaryCtx, related: &str) -> String {
    let mut s = String::new();

    s.push_str("You are a senior code reviewer.\n");
    s.push_str("Only comment on the DIFFED region. Be specific and concise.\n");
    s.push_str("If you need global context (e.g., imports/headers), use the read-only blocks.\n\n");

    // Review policy (rules/)
    let path_for_rules = target_path_for_rules(tgt);
    let rules = compose_rules_for_file(path_for_rules);
    if !rules.trim().is_empty() {
        s.push_str("### Review policy\n");
        s.push_str(&rules);
        s.push_str("\n\n");
    }

    s.push_str("PRIMARY (numbered HEAD lines):\n```code\n");
    s.push_str(&ctx.numbered_snippet);
    s.push_str("```\n");

    if !related.is_empty() {
        s.push_str("\nRELATED (read-only):\n```code\n");
        s.push_str(related);
        s.push_str("\n```\n");
    }
    if let Some(full) = &ctx.full_file_readonly {
        s.push_str(
            "\nFULL FILE (read-only; use ONLY to verify imports/symbol presence or cross-line invariants):\n```code\n",
        );
        s.push_str(full);
        s.push_str("\n```\n");
    }

    s.push_str("\nALLOWED_ANCHORS (inclusive line ranges in the same file):\n");
    for a in &ctx.allowed_anchors {
        s.push_str(&format!("- {}-{}\n", a.start, a.end));
    }

    s.push_str(
        r#"
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


Rules:

- Anchors must be inside ALLOWED_ANCHORS. Prefer _added_ lines when possible.

- Do not mention line numbers in BODY unless they match ANCHOR exactly.

- Do not propose edits outside the anchored lines.

- Prefer minimal, safe changes; avoid speculative or non-applicable diffs.

- If you cannot propose a correct patch, omit PATCH and just explain the issue.
    "#,
    );

    s.push_str("\nTARGET PREVIEW:\n`\n");
    s.push_str(&tgt.preview);
    s.push_str("\n`\n");

    s
}

/// Build refine prompt for SLOW model; keeps the same strict format.
pub fn build_refine_prompt(
    maybe_prev: Option<&crate::review::policy::ParsedFinding>,
    tgt: &MappedTarget,
    ctx: &PrimaryCtx,
    related: &str,
) -> String {
    let mut s = String::new();
    s.push_str("Refine the draft below while preserving the STRICT format.\n");
    s.push_str("Increase precision, keep anchors valid, and remove any speculation.\n\n");

    if let Some(prev) = maybe_prev {
        s.push_str("PREVIOUS DRAFT:\n```\n");
        s.push_str(&prev.raw_block);
        s.push_str("\n```\n\n");
    }

    s.push_str(&build_strict_prompt(tgt, ctx, related));
    s
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
