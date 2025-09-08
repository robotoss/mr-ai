//! Prompt telemetry: safe logging/dumping of the exact LLM prompts.
//!
//! ## What it does
//! - Dumps full prompts to files under `code_data/mr_tmp/<short_sha>/prompts/{fast|slow}/...`.
//! - Emits a concise DEBUG log line (length, token estimate, file path).
//! - Optionally **redacts secrets** (Bearer tokens, GitLab PAT, etc.).
//! - Supports truncation for huge prompts (configurable via env).
//!
//! ## Env flags
//! - `MR_REVIEWER_LOG_PROMPTS` (bool): enable logging/dump (default: false)
//! - `MR_REVIEWER_PROMPT_REDACT` (bool): redact secrets (default: true)
//! - `MR_REVIEWER_PROMPT_MAX_CHARS` (usize): 0 = no truncation (default: 0)
//! - `MR_REVIEWER_PROMPT_ECHO_THRESHOLD` (usize): echo full prompt to log if len <= threshold (default: 0)

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::map::MappedTarget;

/// Returns `true` if the given env var is set to a truthy value ("1", "true", "yes", "on").
fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn env_usize(name: &str, default_: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default_)
}

/// Make a safe filename segment from a repo path (remove dirs & separators).
fn sanitize_path_for_name(p: &str) -> String {
    let s = p.replace('\\', "/");
    s.split('/')
        .filter(|seg| !seg.is_empty())
        .last()
        .unwrap_or("-")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build `code_data/mr_tmp/<short_sha>/prompts/<stage>/...`
fn dump_dir(head_sha: &str, stage: &str) -> PathBuf {
    let short = if head_sha.len() >= 12 {
        &head_sha[..12]
    } else {
        head_sha
    };
    Path::new("code_data")
        .join("mr_tmp")
        .join(short)
        .join("prompts")
        .join(stage)
}

/// Redact obvious secrets (best-effort; keep it conservative).
fn redact_secrets(mut s: String) -> String {
    // GitLab PAT like glpat-..., Bearer tokens, generic long tokens
    let pats = &[
        r"(?i)glpat-[A-Za-z0-9\.\-_]{16,}",
        r"(?i)\bBearer\s+[A-Za-z0-9\-_\.=]{16,}",
        r"(?i)\btoken\s*[:=]\s*[A-Za-z0-9\-_\.=]{16,}",
        r"(?i)Authorization:\s*Bearer\s+[A-Za-z0-9\-_\.=]{16,}",
    ];
    for p in pats {
        let re = Regex::new(p).unwrap();
        s = re.replace_all(&s, "[REDACTED]").into_owned();
    }
    s
}

/// Optionally truncate the prompt to at most `max_chars`, keeping suffix note.
fn maybe_truncate(s: String, max_chars: usize) -> (String, bool) {
    if max_chars == 0 || s.chars().count() <= max_chars {
        return (s, false);
    }
    let mut out = String::new();
    for ch in s.chars().take(max_chars) {
        out.push(ch);
    }
    out.push_str("\n\n[... TRUNCATED ...]\n");
    (out, true)
}

/// Write the prompt to a file and emit a concise DEBUG log line.
/// `stage` is "fast" or "slow".
pub fn dump_prompt_for_target(
    head_sha: &str,
    idx: usize,
    stage: &str,
    tgt: &MappedTarget,
    prompt: &str,
    prompt_tokens_approx: usize,
) {
    if !env_flag("MR_REVIEWER_LOG_PROMPTS") {
        return;
    }

    let redact = env_flag("MR_REVIEWER_PROMPT_REDACT")
        || std::env::var("MR_REVIEWER_PROMPT_REDACT").is_err();
    let max_chars = env_usize("MR_REVIEWER_PROMPT_MAX_CHARS", 0);
    let echo_threshold = env_usize("MR_REVIEWER_PROMPT_ECHO_THRESHOLD", 0);

    // Prepare content
    let mut content = prompt.to_string();
    if redact {
        content = redact_secrets(content);
    }
    let (content, truncated) = maybe_truncate(content, max_chars);

    // File path
    let safe_name = sanitize_path_for_name(match &tgt.target {
        crate::map::TargetRef::Line { path, .. }
        | crate::map::TargetRef::Range { path, .. }
        | crate::map::TargetRef::Symbol { path, .. }
        | crate::map::TargetRef::File { path } => path,
        crate::map::TargetRef::Global => "",
    });

    let dir = dump_dir(head_sha, stage);
    let _ = fs::create_dir_all(&dir);
    let file = dir.join(format!("{:03}_{}_{}.txt", idx, safe_name, stage));
    let _ = fs::write(&file, &content);

    // Concise log
    debug!(
        "prompt[{}] idx={} file={} len={} tokens≈{} truncated={}",
        stage,
        idx,
        file.display(),
        content.chars().count(),
        prompt_tokens_approx,
        truncated
    );

    // Optional echo to log (small prompts only)
    if echo_threshold > 0 && content.chars().count() <= echo_threshold {
        debug!("prompt[{}] idx={} >>>\n{}\n<<<", stage, idx, content);
    }
}
