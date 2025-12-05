//! Prompt builder for the pre-review planning phase.
//!
//! This builder constructs a compact prompt for each review target
//! that asks the LLM to:
//!   * scan the diff hunk;
//!   * identify potential hypotheses and missing context;
//!   * propose targeted context hints for RAG;
//!   * return a strict JSON object describing the plan.

use std::fmt::Write as FmtWrite;

use crate::diff_model::ReviewTarget;
use crate::git_providers::types::CrBundle;
use crate::rag_layer::TargetRagContext;
use crate::rules::RuleSet;

/// Builds a pre-review planning prompt for a single review target.
///
/// `current` is the hunk we want to analyze as PRIMARY DIFF.
/// `file_targets` are all hunks for the same file (including `current`)
/// so that the model can see moves/related changes in the file.
pub fn build_prereview_prompt(
    bundle: &CrBundle,
    current: &ReviewTarget,
    file_targets: &[&ReviewTarget],
    rules: &RuleSet,
    rag_ctx: Option<&TargetRagContext>,
) -> String {
    let mut buf = String::new();

    // ---------------------------------------------------------------------
    // ROLE
    // ---------------------------------------------------------------------
    buf.push_str(
        "ROLE\n\
         ----\n\
         You are a planning assistant for an automated code review system.\n\
         Your task is NOT to perform the final review.\n\
         Instead, you must:\n\
         - scan the diff hunk;\n\
         - identify potential hypotheses and uncertain areas;\n\
         - propose what additional context should be fetched (via RAG or repository search);\n\
         - prioritize these hypotheses.\n\n\
         SOURCES OF TRUTH\n\
         -----------------\n\
         - The PRIMARY DIFF block (between BEGIN_DIFF and END_DIFF) is the ONLY authoritative view of the change.\n\
         - RELATED DIFF blocks show other hunks from the same file and are READ-ONLY helpers.\n\
           You may use them to detect moved or related code, but you MUST NOT use their lines\n\
           in `anchor_lines`.\n\
         - Any RAG snippets are READ-ONLY and NON-AUTHORITATIVE helpers.\n\
         - You MUST not invent files, functions or behavior that cannot be tied to the diff.\n\n\
         HYPOTHESIS SELECTION\n\
         --------------------\n\
         - Prefer a small number of high-signal hypotheses over many vague ones.\n\
         - Use hypotheses only when you see a realistic chance of a bug, regression or design risk,\n\
           or when you clearly lack context to safely reason about behavior.\n\
         - If the diff looks trivial and safe, return an empty hypothesis list.\n\n",
    );

    // ---------------------------------------------------------------------
    // CHANGE METADATA
    // ---------------------------------------------------------------------
    let author = bundle
        .meta
        .author
        .name
        .clone()
        .or(bundle.meta.author.username.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let description = bundle.meta.description.clone().unwrap_or_default();

    let _ = writeln!(
        &mut buf,
        "CHANGE METADATA\n\
         ---------------\n\
         Project: [{}]\n\
         Title:   {}\n\
         MR/PR:   #{}\n\
         Author:  {}\n\
         URL:     {}",
        bundle.meta.id.project, bundle.meta.title, bundle.meta.id.iid, author, bundle.meta.web_url
    );

    if !description.trim().is_empty() {
        let _ = writeln!(&mut buf, "Description: {}", description.trim());
    }
    buf.push('\n');

    let _ = writeln!(
        &mut buf,
        "TARGET IDENTIFIER (MUST BE COPIED EXACTLY INTO JSON)\n\
         --------------------------------------------------"
    );
    let _ = writeln!(&mut buf, "FILE_PATH: {}", current.file_path);
    let _ = writeln!(&mut buf, "HUNK_INDEX: {}", current.hunk_index);
    buf.push('\n');

    // ---------------------------------------------------------------------
    // PRIMARY DIFF (CURRENT HUNK)
    // ---------------------------------------------------------------------
    let _ = writeln!(
        &mut buf,
        "=== PRIMARY DIFF (HEAD; authoritative) ===\n\
         The following block shows the diff hunk YOU MUST ANALYZE.\n\
         All anchor lines in hypotheses MUST be tied only to lines inside this block.\n\
         BEGIN_DIFF file={} hunk_index={}",
        current.file_path, current.hunk_index
    );

    let _ = writeln!(&mut buf, "{}", current.diff_preview.trim_end());

    buf.push_str("END_DIFF\n\n");

    // ---------------------------------------------------------------------
    // RELATED DIFFS (OTHER HUNKS IN SAME FILE)
    // ---------------------------------------------------------------------
    // We render other hunks for the same file so that the model can see
    // moves/related changes, but it MUST NOT use those lines in anchor_lines.
    let has_other_hunks = file_targets
        .iter()
        .any(|t| t.hunk_index != current.hunk_index);

    if has_other_hunks {
        buf.push_str(
            "=== RELATED DIFF BLOCKS (same file, other hunks) ===\n\
             These blocks show other diff hunks from the same file.\n\
             Use them ONLY to understand moved or related code.\n\
             You MUST NOT use lines from these blocks in `anchor_lines`.\n\n",
        );

        for t in file_targets {
            if t.hunk_index == current.hunk_index {
                continue;
            }

            let _ = writeln!(
                &mut buf,
                "-- RELATED_HUNK file={} hunk_index={}",
                t.file_path, t.hunk_index
            );
            let _ = writeln!(&mut buf, "{}", t.diff_preview.trim_end());
            buf.push('\n');
        }
    }

    // ---------------------------------------------------------------------
    // OPTIONAL RAG CONTEXT
    // ---------------------------------------------------------------------
    if let Some(rag_ctx) = rag_ctx {
        if !rag_ctx.results.is_empty() {
            buf.push_str(
                "=== RAG CONTEXT (READ-ONLY, NON-AUTHORITATIVE) ===\n\
                 The following code snippets were retrieved via semantic search.\n\
                 Treat them as hints about surrounding project structure (e.g. routing, state management).\n\
                 Do NOT claim behavior that cannot be justified by the primary diff and the current file.\n\
                 Do NOT use line numbers from this section in `anchor_lines`.\n\n",
            );

            for (i, r) in rag_ctx.results.iter().enumerate() {
                let _ = writeln!(
                    &mut buf,
                    "-- RAG[{i}] file={} (score: {:.3})",
                    r.file, r.score
                );
                if let Some(snippet) = &r.snippet {
                    buf.push_str(snippet.trim_end());
                    buf.push('\n');
                }
                buf.push('\n');
            }
        }
    }

    // ---------------------------------------------------------------------
    // SHORT RULES SUMMARY
    // ---------------------------------------------------------------------
    if !rules.bullets.is_empty() {
        buf.push_str(
            "=== REVIEW PROFILE (SUMMARY) ===\n\
             The final review will prioritize:\n",
        );
        for b in &rules.bullets {
            let _ = writeln!(&mut buf, "- {b}");
        }
        buf.push('\n');
    }

    // ---------------------------------------------------------------------
    // FINAL INSTRUCTIONS + JSON SCHEMA
    // ---------------------------------------------------------------------
    buf.push_str(
        r#"=== FINAL INSTRUCTIONS ===
Perform a planning pass for this diff hunk.

You MUST:
- identify only those hypotheses that are clearly motivated by the PRIMARY DIFF;
- for each hypothesis, provide:
  - `anchor_lines` with exact diff lines from the PRIMARY DIFF block;
  - a short `title`;
  - a concrete `question` that the final review should answer;
  - a `priority` (High, Medium, Low);
  - a `kind` (MissingContext, PossibleBug, DesignQuestion);
  - one or more `required_context` entries describing what needs to be fetched.

ANCHORS AND DIFF LINES
----------------------
- `anchor_lines` MUST contain exact lines copied from the PRIMARY DIFF block,
  including:
  - any leading '+' / '-' marker,
  - any leading spaces,
  - the numeric prefix,
  - the '|' separator and the code that follows.
- Do NOT invent or reformat lines.
- Do NOT use lines from RELATED DIFF blocks or RAG sections in `anchor_lines`.

OUTPUT FORMAT (STRICT JSON)
---------------------------
Return ONLY a single JSON object with the following structure:

{
  "file_path": "<exact FILE_PATH from above>",
  "hunk_index": <exact HUNK_INDEX from above>,
  "hypotheses": [
    {
      "id": "H1",
      "anchor_lines": [
        "<exact diff line from the PRIMARY DIFF block>",
        "<optional second line if the hypothesis spans multiple lines>"
      ],
      "priority": "High" | "Medium" | "Low",
      "kind": "MissingContext" | "PossibleBug" | "DesignQuestion",
      "title": "<short one-line summary>",
      "question": "<concrete question that must be answered by the final review>",
      "required_context": [
        {
          "kind": "<short category such as 'RoutingConfig', 'Usage', 'TypeDef', 'ConfigFile', 'Other'>",
          "description": "<one or two sentences describing what should be fetched>",
          "query": "<suggested free-text query for code search or RAG>",
          "tags": ["tag1", "tag2"],
          "suggested_files": ["path/pattern/one.dart", "lib/*router*.dart"]
        }
      ]
    }
  ]
}

HARD CONSTRAINTS
----------------
- Output MUST be valid JSON:
  - no comments,
  - no trailing commas,
  - no extra text before or after the JSON object.
- You MUST copy `file_path` and `hunk_index` EXACTLY from FILE_PATH and HUNK_INDEX shown above.
- If there are NO hypotheses, return:

{
  "file_path": "<same FILE_PATH as above>",
  "hunk_index": <same HUNK_INDEX as above>,
  "hypotheses": []
}
"#,
    );

    buf
}
