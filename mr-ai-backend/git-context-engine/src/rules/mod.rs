//! Review rules and profiles used when constructing AI prompts.

pub mod builtin;

use serde::{Deserialize, Serialize};

/// A single review rule.
///
/// Rules are plain-text instructions for the AI model. They can cover
/// correctness, security, style, performance and any project-specific
/// constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Machine-readable identifier, stable across releases.
    pub id: String,
    /// Human-readable short title.
    pub title: String,
    /// Full text of the rule.
    pub body: String,
}

/// A set of review rules grouped under a logical profile name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSet {
    /// Logical profile name (for example: "default", "security").
    pub name: String,
    /// Ordered list of rules that will be rendered into the prompt.
    pub rules: Vec<Rule>,
}

impl RuleSet {
    /// Creates an empty rule set with the given name.
    pub fn empty(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            rules: Vec::new(),
        }
    }

    /// Returns `true` if the rule set has no rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}
