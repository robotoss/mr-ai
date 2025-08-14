//! Rich AST node model used throughout the pipeline.
//!
//! The node stores normalized metadata necessary for building language-aware
//! graphs and high-quality RAG payloads. It is intentionally language-agnostic,
//! with `kind`, `visibility`, and `annotations` as enums to keep the schema stable.

use crate::model::{language::LanguageKind, span::Span};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Visibility markers across languages. Not all variants apply to every language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Crate,
    Package,
}

/// Coarse-grained AST node kind.
/// Extend conservatively; prefer to keep the set stable for payload consumers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AstKind {
    File,
    Module,
    Package,
    Class,
    Mixin,
    Enum,
    Extension,
    ExtensionType,
    Interface,
    TypeAlias,
    Trait,
    Impl,
    Function,
    Method,
    Field,
    Variable,
    Import,
    Export,
    Part,
    PartOf,
    Macro,
}

/// Generic annotation/decorator/attribute marker.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Annotation {
    /// Annotation name, e.g. `derive`, `Injectable`, `deprecated`.
    pub name: String,
    /// Raw text/value if available, language-specific.
    #[serde(default)]
    pub value: Option<String>,
}

/// Unified AST node used as graph vertex and as basis for RAG payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstNode {
    /// Deterministic unique id for the symbol, recommended as primary key.
    /// Use UUIDv5 with a stable namespace for cross-machine stability.
    pub symbol_id: String,

    /// Human name as appears in the source code (class/function/etc).
    pub name: String,

    /// AST kind (class/function/method/import/…).
    pub kind: AstKind,

    /// Language of the file this node belongs to.
    pub language: LanguageKind,

    /// Normalized path relative to repository root (portable key).
    pub file: String,

    /// Source span (lines/bytes).
    pub span: Span,

    /// Container chain, from outermost to innermost (e.g., ["MyClass","Inner"]).
    #[serde(default)]
    pub owner_path: Vec<String>,

    /// Fully-qualified name (project::module::Type::method).
    #[serde(default)]
    pub fqn: String,

    /// Visibility, where applicable.
    #[serde(default)]
    pub visibility: Option<Visibility>,

    /// Raw signature (as in source), if captured.
    #[serde(default)]
    pub signature: Option<String>,

    /// Docstring/comment attached to this node, if any.
    #[serde(default)]
    pub doc: Option<String>,

    /// Decorators/annotations/attributes.
    #[serde(default)]
    pub annotations: Vec<Annotation>,

    /// For import nodes: optional alias; for other nodes may remain None.
    #[serde(default)]
    pub import_alias: Option<String>,

    /// Best-effort resolved absolute/normalized target for import/export/part directives.
    #[serde(default)]
    pub resolved_target: Option<String>,

    /// True if file is recognized as generated (based on glob patterns).
    #[serde(default)]
    pub is_generated: bool,
}

impl AstNode {
    /// Construct a minimal file-level node as a placeholder.
    ///
    /// NOTE: This constructor uses a *temporary* UUIDv5 with the nil namespace.
    /// In production, prefer [`AstNode::with_symbol_id`] to inject a proper namespace.
    pub fn file_node_stub(language: LanguageKind, file: String) -> Self {
        let kind = AstKind::File;
        let span = Span::new(0, 0, 0, 0);
        let key = format!("{}|{}|{}|{}|{}", language, file, 0, 0, "file");
        let id = Uuid::new_v5(&Uuid::nil(), key.as_bytes());
        Self {
            symbol_id: id.to_string(),
            name: file.clone(),
            kind,
            language,
            file,
            span,
            owner_path: Vec::new(),
            fqn: String::new(),
            visibility: None,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            import_alias: None,
            resolved_target: None,
            is_generated: false,
        }
    }

    /// Create the node with a deterministic UUIDv5 using the provided namespace.
    ///
    /// The `stable_key` should be a reproducible composite string such as:
    /// `{lang}|{norm_path}|{start_byte}-{end_byte}|{name}|{kind}`.
    pub fn with_symbol_id(mut self, namespace: Uuid, stable_key: &str) -> Self {
        self.symbol_id = Uuid::new_v5(&namespace, stable_key.as_bytes()).to_string();
        self
    }

    /// Compute a recommended stable-key for UUIDv5 based on core attributes.
    pub fn compute_stable_key(&self) -> String {
        format!(
            "{}|{}|{}-{}|{}|{:?}",
            self.language,
            self.file,
            self.span.start_byte,
            self.span.end_byte,
            self.name,
            self.kind
        )
    }

    /// A simple, line-based metric (LOC) derived from span.
    pub fn loc(&self) -> usize {
        self.span.line_count()
    }
}
