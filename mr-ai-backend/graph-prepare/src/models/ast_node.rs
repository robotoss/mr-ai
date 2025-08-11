use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ASTNode {
    pub name: String,
    pub node_type: String, // "file","class","function","method","import","export","part",...
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,

    /// Owner class for methods, if any.
    #[serde(default)]
    pub owner_class: Option<String>,

    /// For import nodes: optional alias (e.g., `import 'x.dart' as api;`)
    #[serde(default)]
    pub import_alias: Option<String>,

    /// For import/export/part nodes: best-effort resolved absolute file path.
    #[serde(default)]
    pub resolved_target: Option<String>,
}
