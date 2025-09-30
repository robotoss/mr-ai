use std::collections::{BTreeMap, HashSet};

use crate::types::{ImportUse, OriginKind};

/// Language-agnostic per-file AST extras consumed by the LSP merger.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FileAstExtras {
    /// Normalized imports (origin + label + identifier).
    pub imports: Vec<ImportUse>,
    /// Qualified or normalized type/symbol usages.
    pub uses: Vec<String>,
    /// Optional free-form tags derived from AST (e.g., "flutter_widget", "router").
    pub tags: Vec<String>,
    /// Optional language/framework-specific facts (namespaced keys).
    pub facts: BTreeMap<String, serde_json::Value>,
}

impl FileAstExtras {
    /// Sort & dedup to keep downstream merges deterministic.
    pub fn normalize(&mut self) {
        // imports: dedup by (origin,label,identifier) without requiring Ord
        let mut seen = HashSet::<(u8, String, String)>::new();
        self.imports.retain(|iu| {
            let key = (
                origin_key(iu.origin),
                iu.label.clone(),
                iu.identifier.clone(),
            );
            seen.insert(key)
        });
        self.imports
            .sort_by(|a, b| a.label.cmp(&b.label).then(a.identifier.cmp(&b.identifier)));

        self.uses.sort();
        self.uses.dedup();

        self.tags.sort();
        self.tags.dedup();
    }
}

fn origin_key(o: OriginKind) -> u8 {
    match o {
        OriginKind::Sdk => 0,
        OriginKind::Package => 1,
        OriginKind::Local => 2,
        OriginKind::Unknown => 3,
    }
}

// ---- Adapter from your Dart AST summary ------------------------------------

use super::ast::AstFile;

/// Convert Dart-specific `AstFile` into generic `FileAstExtras`.
impl From<AstFile> for FileAstExtras {
    fn from(a: AstFile) -> Self {
        FileAstExtras {
            imports: a.imports,
            uses: a.uses,
            tags: Vec::new(),       // AstFile doesn't provide tags
            facts: BTreeMap::new(), // AstFile doesn't provide facts
        }
    }
}

/// Optional: zero-copy-ish conversion when you have a reference.
impl From<&AstFile> for FileAstExtras {
    fn from(a: &AstFile) -> Self {
        FileAstExtras {
            imports: a.imports.clone(),
            uses: a.uses.clone(),
            tags: Vec::new(),
            facts: BTreeMap::new(),
        }
    }
}
