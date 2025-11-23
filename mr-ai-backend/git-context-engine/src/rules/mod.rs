//! Rule engine for git-context-engine.
//!
//! Responsibilities:
//!   * `RuleSet` – logical review rule bundle,
//!   * loading markdown rules from `rules/global/*.md` and `rules/<lang>/*.md`,
//!   * composing built-in and file-based rules into a single prompt string.
//!
//! Rules root directory is resolved as:
//!   1. `GIT_CONTEXT_ENGINE_RULES_DIR` env var,
//!   2. `./rules` fallback.
pub mod builtin;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// Logical set of review rules that will be rendered into the prompt.
#[derive(Debug, Clone)]
pub struct RuleSet {
    /// Human-readable profile name (e.g. "default", "security").
    pub profile_name: String,
    /// Built-in rule bullets that are compiled into the binary.
    pub bullets: Vec<String>,
}

/// Resolve the root directory for markdown rule files.
///
/// Precedence:
///   1. `GIT_CONTEXT_ENGINE_RULES_DIR`
///   2. `./rules`
pub fn rules_root() -> PathBuf {
    std::env::var("GIT_CONTEXT_ENGINE_RULES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("rules"))
}

/// Very simple language folder detection based on file extension.
///
/// Used to map file paths to a directory like `rules/dart`, `rules/rust`, etc.
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

/// Read all `*.md` files under the given directory, sort by file name,
/// and concatenate them into a single string.
///
/// Returns `None` if the directory does not exist, cannot be read,
/// or contains no readable markdown files.
fn read_dir_concat(dir: &Path) -> Option<String> {
    if !dir.exists() {
        debug!("rules: dir does not exist, skipping: {}", dir.display());
        return None;
    }

    let mut files = match fs::read_dir(dir) {
        Ok(r) => r
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
            .collect::<Vec<_>>(),
        Err(e) => {
            warn!("rules: failed to read dir {}: {e}", dir.display());
            return None;
        }
    };

    files.sort();

    let mut buf = String::new();
    for p in files {
        match fs::read_to_string(&p) {
            Ok(s) => {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                buf.push_str(&format!("### {}\n\n{}\n\n", name, s));
            }
            Err(e) => {
                warn!("rules: failed to read file {}: {e}", p.display());
            }
        }
    }

    if buf.trim().is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// Compose the final rule text for a given file path.
///
/// Order:
///   1. Built-in bullets from `RuleSet`,
///   2. `rules/global/*.md`,
///   3. `rules/<lang>/*.md` (language inferred from file extension).
pub fn compose_rules_for_file(path: &str, builtin: &RuleSet) -> String {
    let root = rules_root();
    debug!(
        "rules: composing rules for path='{}', root='{}'",
        path,
        root.display()
    );

    let mut sections = Vec::<String>::new();

    // 1. Built-in rule bullets.
    if !builtin.bullets.is_empty() {
        let mut buf = String::new();
        buf.push_str("## Built-in rules\n\n");
        for b in &builtin.bullets {
            buf.push_str("- ");
            buf.push_str(b);
            buf.push('\n');
        }
        sections.push(buf);
    }

    // 2. Global rules.
    let global_dir = root.join("global");
    if let Some(text) = read_dir_concat(&global_dir) {
        info!(
            "rules: loaded global rules from {} ({} chars)",
            global_dir.display(),
            text.len()
        );
        sections.push(format!("## Global rules\n\n{}", text));
    } else {
        debug!("rules: no global rules in {}", global_dir.display());
    }

    // 3. Language-specific rules.
    if !path.is_empty() {
        let lang = detect_lang_folder(path);
        let lang_dir = root.join(lang);
        if let Some(text) = read_dir_concat(&lang_dir) {
            info!(
                "rules: loaded language rules '{}' from {} ({} chars)",
                lang,
                lang_dir.display(),
                text.len()
            );
            sections.push(format!("## Language rules ({})\n\n{}", lang, text));
        } else {
            debug!(
                "rules: no language rules for '{}' in {}",
                lang,
                lang_dir.display()
            );
        }
    }

    if sections.is_empty() {
        warn!(
            "rules: no applicable rules for file '{}' (root={})",
            path,
            root.display()
        );
        String::new()
    } else {
        sections.join("\n\n---\n\n")
    }
}
