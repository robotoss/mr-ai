//! Dart-specific dependency linker for monorepos.
//!
//! What this builder does:
//! - Creates **file** nodes for every `.dart` file discovered in AST facts.
//! - Adds **declares** edges from each file to its **class/function** declarations.
//! - Adds **imports / exports / part** edges **between files** by resolving URIs:
//!     * `package:<name>/path.dart` → `<pkg-root>/lib/path.dart`
//!     * relative `../x.dart` → resolved from source file directory
//!     * `dart:<...>` is ignored (std library)
//! - Monorepo-aware package resolution by scanning **all `pubspec.yaml`** files:
//!     * `name:` → `<that-pubspec-dir>/lib`
//!     * `dependencies/dev_dependencies/dependency_overrides` with `{ path: ... }`
//!       → `<base_dir>/<path>/lib`
//!
//! Notes:
//! - We strictly **avoid** adding import statements as nodes; imports are only **edges between files**.
//! - Paths are normalized relative to the repository root so that node keys match consistently.
//! - Optionally (enabled by default), we **flatten re-exports**, adding `imports_via_export`
//!   edges to the actual exported files for better high-level topology reading.

use crate::{graphs::GraphEdge, models::ast_node::ASTNode};
use anyhow::{Context, Result};
use dunce;
use petgraph::graph::{Graph, NodeIndex};
use serde::Deserialize;
use serde_yml::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

/// Toggle whether to add direct edges from importers to re-exported files.
/// When `true`, for an edge `A --imports--> façade.dart` and `façade.dart --exports--> impl.dart`,
/// we also add `A --imports_via_export--> impl.dart`.
const FLATTEN_EXPORTS: bool = true;

/// Toggle whether to skip commonly generated files in the graph.
const EXCLUDE_GENERATED: bool = true;

/// Build a Dart file-level graph from AST nodes extracted from `.dart` files.
pub fn build_graph_dart(root: &str, nodes: &[ASTNode]) -> Result<Graph<ASTNode, GraphEdge>> {
    let rootp = Path::new(root);

    // 1) Keep only Dart nodes (optionally skip generated files) and precompute normalized file keys.
    let dart_nodes: Vec<&ASTNode> = nodes
        .iter()
        .filter(|n| n.file.ends_with(".dart"))
        .filter(|n| !(EXCLUDE_GENERATED && is_generated_file(&n.file)))
        .collect();

    // Set of all file keys we will need to create as **file** nodes.
    let mut files: HashSet<String> = dart_nodes
        .iter()
        .map(|n| norm_repo_str(rootp, Path::new(&n.file)))
        .collect();

    // 2) Build a registry of packages for `package:` URI resolution.
    let pkg = PackageRegistry::from_root(root)?;

    // 3) Gather file-to-file edges from import/export/part directives.
    let mut edges: Vec<(String, String, String)> = Vec::new(); // (src_key, dst_key, label)
    let mut exported_from: HashMap<String, Vec<String>> = HashMap::new(); // façade_key -> [impl_keys]

    for n in &dart_nodes {
        match n.node_type.as_str() {
            "import" | "export" | "part" => {
                // Normalize the source file path once.
                let src_key = norm_repo_str(rootp, Path::new(&n.file));
                // Resolve the URI into a repository path and normalize it.
                if let Some(dst_path) = resolve_dart_uri_path(rootp, &n.file, &n.name, &pkg) {
                    let dst_key = norm_repo_str(rootp, &dst_path);
                    files.insert(dst_key.clone());

                    let label = match n.node_type.as_str() {
                        "export" => "exports",
                        "part" => "part",
                        _ => "imports",
                    }
                    .to_string();

                    if label == "exports" {
                        exported_from
                            .entry(src_key.clone())
                            .or_default()
                            .push(dst_key.clone());
                    }

                    edges.push((src_key, dst_key, label));
                }
            }
            _ => {}
        }
    }

    // Optionally flatten re-exports for better readability at the project level.
    if FLATTEN_EXPORTS {
        let mut extra: Vec<(String, String, String)> = Vec::new();
        for (src, dst, label) in edges.iter() {
            if label == "imports" {
                if let Some(rexports) = exported_from.get(dst) {
                    for impl_key in rexports {
                        extra.push((
                            src.clone(),
                            impl_key.clone(),
                            "imports_via_export".to_string(),
                        ));
                    }
                }
            }
        }
        edges.extend(extra);
    }

    // 4) Build the graph with **file** nodes first so we can attach declares/imports edges.
    let mut g: Graph<ASTNode, GraphEdge> = Graph::new();
    let mut file_idx: HashMap<String, NodeIndex> = HashMap::new();

    for f in files {
        let idx = g.add_node(ASTNode {
            name: f.clone(), // use normalized key as the node "name"
            node_type: "file".into(),
            file: f.clone(), // keep the same for convenience
            start_line: 0,
            end_line: 0,
        });
        file_idx.insert(f, idx);
    }

    // 5) Add declarations and connect them with `declares` from their file node.
    for n in &dart_nodes {
        if n.node_type == "function" || n.node_type == "class" {
            let idx = g.add_node((*n).clone());

            // Normalize the file key in the same manner as for file nodes.
            let fkey = norm_repo_str(rootp, Path::new(&n.file));
            if let Some(&fidx) = file_idx.get(&fkey) {
                g.add_edge(fidx, idx, GraphEdge("declares".into()));
            }
        }
    }

    // 6) Add imports/exports/part edges between file nodes.
    for (src_key, dst_key, label) in edges {
        if let (Some(&s), Some(&d)) = (file_idx.get(&src_key), file_idx.get(&dst_key)) {
            g.add_edge(s, d, GraphEdge(label));
        }
    }

    Ok(g)
}

/// Resolve a Dart URI from a directive to a path inside the repository.
/// - `package:pkg/path.dart` -> `<pkg-root>/lib/path.dart` (using PackageRegistry)
/// - relative (`../x.dart` or `./y.dart`) -> resolved from the source file directory
/// - `dart:` URIs -> None (std library)
fn resolve_dart_uri_path(
    root: &Path,
    src_file: &str,
    uri: &str,
    pkg: &PackageRegistry,
) -> Option<PathBuf> {
    if uri.starts_with("dart:") {
        return None;
    }
    if uri.starts_with("package:") {
        // package:<name>/<rel>
        let rest = &uri["package:".len()..];
        let mut parts = rest.splitn(2, '/');
        let pkg_name = parts.next()?;
        let rel = parts.next().unwrap_or("");
        let lib_dir = pkg.lookup(pkg_name)?;
        return Some(lib_dir.join(rel));
    }
    // Treat it as a relative path.
    let base = Path::new(src_file).parent().unwrap_or(root);
    Some(base.join(uri))
}

/// Normalize any file path to a stable string key relative to the repo root.
///
/// Steps:
/// 1) Make absolute (join with `root` when needed).
/// 2) Best-effort canonicalize (resolves `.`/`..`, symlinks when possible).
/// 3) Strip the `root` prefix so keys remain stable if the repo moves.
/// 4) Convert to a platform-appropriate string.
fn norm_repo_str(root: &Path, p: &Path) -> String {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let abs = dunce::canonicalize(&abs).unwrap_or(abs);
    let rel = abs.strip_prefix(root).unwrap_or(&abs);
    rel.to_string_lossy().into_owned()
}

/// Heuristic to skip generated files (to reduce noise in the graph).
fn is_generated_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".g.dart")
        || lower.ends_with(".freezed.dart")
        || lower.ends_with(".gr.dart")
        || lower.ends_with(".chopper.dart")
        || lower.ends_with(".swagger.dart")
        || lower.contains("/gen/")      // common build dir
        || lower.contains("/generated/") // common build dir
}

/// Registry mapping `package:` names to `<pkg-root>/lib`, built by scanning pubspecs.
///
/// Strategy:
/// - For every `pubspec.yaml`:
///   * read `name:` → map `name -> <pubspec-dir>/lib`
///   * inspect `dependencies`, `dev_dependencies`, `dependency_overrides`;
///     if an entry has `{ path: ... }`, add `dep_name -> <base_dir>/<path>/lib`
///
/// This allows resolving `package:<name>/...` URIs across a monorepo with local path deps.
struct PackageRegistry {
    packages: HashMap<String, PathBuf>,
}

impl PackageRegistry {
    fn from_root(root: &str) -> Result<Self> {
        #[derive(Deserialize, Default)]
        struct PubspecName {
            name: Option<String>,
        }

        let mut map = HashMap::<String, PathBuf>::new();

        for e in WalkDir::new(root).into_iter().filter_map(Result::ok) {
            let p = e.path();
            if p.file_name().and_then(|s| s.to_str()) != Some("pubspec.yaml") {
                continue;
            }

            let base_dir = p.parent().unwrap_or(Path::new(root));
            let content =
                fs::read_to_string(p).with_context(|| format!("read pubspec: {}", p.display()))?;

            // name -> <pubspec-dir>/lib
            if let Ok(meta) = serde_yml::from_str::<PubspecName>(&content) {
                if let Some(name) = meta.name {
                    map.entry(name).or_insert(base_dir.join("lib"));
                }
            }

            // path-based deps from this pubspec
            let val: Value = serde_yml::from_str(&content).unwrap_or(Value::Null);
            collect_path_deps_into(base_dir, &val, "dependencies", &mut map);
            collect_path_deps_into(base_dir, &val, "dev_dependencies", &mut map);
            collect_path_deps_into(base_dir, &val, "dependency_overrides", &mut map);
        }

        Ok(Self { packages: map })
    }

    fn lookup(&self, name: &str) -> Option<PathBuf> {
        self.packages.get(name).cloned()
    }
}

/// Extract `{ path: <rel-or-abs> }` from a dependency map and push into registry.
/// Example:
/// ```yaml
/// dependencies:
///   home_feature:
///     path: packages/home_feature
/// ```
fn collect_path_deps_into(
    base_dir: &Path,
    root: &serde_yml::Value,
    key: &str,
    out: &mut HashMap<String, PathBuf>,
) {
    let Some(deps) = root.get(key) else { return };
    let Some(map) = deps.as_mapping() else { return };

    for (k, v) in map {
        let Some(dep_name) = k.as_str() else { continue };
        if let Some(dep_map) = v.as_mapping() {
            if let Some(path_val) = dep_map.get(&serde_yml::Value::from("path")) {
                if let Some(path_str) = path_val.as_str() {
                    let dir = if Path::new(path_str).is_absolute() {
                        PathBuf::from(path_str)
                    } else {
                        base_dir.join(path_str)
                    };
                    out.entry(dep_name.to_string()).or_insert(dir.join("lib"));
                }
            }
        }
    }
}
