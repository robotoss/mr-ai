//! Built-in default rule set for git-context-engine.
//!
//! These rules are always present and then extended by markdown rules
//! loaded from `rules/global/*.md` and `rules/<lang>/*.md`.

use crate::context::rules::RuleSet;

/// Default review profile used when no custom profile is configured.
pub fn default_rule_set() -> RuleSet {
    RuleSet {
        profile_name: "default".to_string(),
        bullets: vec![
            "Correctness and robustness: Check that the code is logically correct, handles edge cases, and does not introduce obvious bugs or regressions.".to_string(),
            "Readability and maintainability: Check that the code is easy to read and maintain: naming is clear, duplication is minimized, and complexity is kept under control.".to_string(),
            "Safety and security: Watch for security issues, unsafe patterns, and untrusted input handling. Highlight any potential vulnerabilities.".to_string(),
            "Project style and conventions: Check whether the code follows common or project-specific style and formatting conventions. You may reference linters or style guides when relevant.".to_string(),
        ],
    }
}
