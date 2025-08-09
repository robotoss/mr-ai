use serde::{Deserialize, Serialize};

/// Represents a node in the AST: a function, class or import/export.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ASTNode {
    /// Identifier of the node (function name, class name, or import text).
    pub name: String,
    /// Type of the node: "function", "class", or "import".
    pub node_type: String,
    /// File path where this node was found.
    pub file: String,
    /// Starting line number (1-based).
    pub start_line: usize,
    /// Ending line number (1-based).
    pub end_line: usize,
}
