//! Built-in review rules used by default.

use crate::rules::{Rule, RuleSet};

/// Returns the default built-in rule set.
///
/// This profile is intentionally generic and can be replaced or extended
/// by the host application.
pub fn default_rule_set() -> RuleSet {
    let mut rules = Vec::<Rule>::new();

    rules.push(Rule {
        id: "correctness".into(),
        title: "Correctness and robustness".into(),
        body: "Check that the code is logically correct, handles edge cases, and \
            does not introduce obvious bugs or regressions."
            .into(),
    });

    rules.push(Rule {
        id: "readability".into(),
        title: "Readability and maintainability".into(),
        body: "Check that the code is easy to read and maintain: \
               naming is clear, duplication is minimized, and complexity \
               is kept under control."
            .into(),
    });

    rules.push(Rule {
        id: "safety".into(),
        title: "Safety and security".into(),
        body: "Watch for security issues, unsafe patterns, and untrusted input \
            handling. Highlight any potential vulnerabilities."
            .into(),
    });

    rules.push(Rule {
        id: "style".into(),
        title: "Project style and conventions".into(),
        body: "Check whether the code follows common or project-specific style \
               and formatting conventions. You may reference linters or style \
               guides when relevant."
            .into(),
    });

    RuleSet {
        name: "default".into(),
        rules,
    }
}
