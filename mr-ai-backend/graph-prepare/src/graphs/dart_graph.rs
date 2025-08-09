//! Dart-specific dependency linker:
//! - file-level graph with `imports` / `exports` / `part` edges
//! - `declares` edges from file nodes to functions/classes declared inside
//! - monorepo-aware: resolves `package:` using PackageRegistry (pubspec + path deps)

use crate::{graphs::edge::GraphEdge, models::ast_node::ASTNode};
use anyhow::{Context, Result};
use petgraph::graph::{Graph, NodeIndex};
use serde::Deserialize;
use serde_yml::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

/// Build a Dart file-level graph from AST nodes extracted from `.dart` files.
pub fn build_graph_dart(root: &str, nodes: &[ASTNode]) -> Result<Graph<ASTNode, GraphEdge>> {
    let dart_nodes: Vec<&ASTNode> = nodes.iter().filter(|n| n.file.ends_with(".dart")).collect();

    let mut files: HashSet<String> = dart_nodes.iter().map(|n| n.file.clone()).collect();

    // Resolve package uris with monorepo awareness
    let pkg = PackageRegistry::from_root(root)?;

    // Collect file-to-file edges
    let mut edges: Vec<(String, String, String)> = Vec::new();

    for n in &dart_nodes {
        match n.node_type.as_str() {
            "import" | "export" | "part" => {
                if let Some(dst) = resolve_dart_uri(root, &n.file, &n.name, &pkg) {
                    files.insert(dst.clone());
                    let label = match n.node_type.as_str() {
                        "export" => "exports",
                        "part" => "part",
                        _ => "imports",
                    };
                    edges.push((n.file.clone(), dst, label.to_string()));
                }
            }
            _ => {}
        }
    }

    // Build the graph with file nodes first
    let mut g: Graph<ASTNode, GraphEdge> = Graph::new();
    let mut file_idx: HashMap<String, NodeIndex> = HashMap::new();

    for f in files.iter() {
        let idx = g.add_node(ASTNode {
            name: f.clone(),
            node_type: "file".into(),
            file: f.clone(),
            start_line: 0,
            end_line: 0,
        });
        file_idx.insert(f.clone(), idx);
    }

    // Add declarations (functions/classes) and link them from their file
    for n in &dart_nodes {
        if n.node_type == "function" || n.node_type == "class" {
            let idx = g.add_node((*n).clone());
            if let Some(&fidx) = file_idx.get(&n.file) {
                g.add_edge(fidx, idx, GraphEdge("declares".into()));
            }
        }
    }

    // Add file-to-file edges
    for (src, dst, label) in edges {
        if let (Some(&s), Some(&d)) = (file_idx.get(&src), file_idx.get(&dst)) {
            g.add_edge(s, d, GraphEdge(label));
        }
    }

    Ok(g)
}

/// Resolve Dart import/export/part URI into a repo path.
/// - `package:pkg/path.dart` -> `<pkg-root>/lib/path.dart`
/// - relative path -> resolved from source file directory
/// - `dart:` -> None
fn resolve_dart_uri(
    root: &str,
    src_file: &str,
    uri: &str,
    pkg: &PackageRegistry,
) -> Option<String> {
    if uri.starts_with("dart:") {
        return None;
    }
    if uri.starts_with("package:") {
        let rest = &uri["package:".len()..];
        let mut parts = rest.splitn(2, '/');
        let pkg_name = parts.next()?;
        let rel = parts.next().unwrap_or("");
        let lib_dir = pkg.lookup(pkg_name)?;
        let dst = lib_dir.join(rel);
        return Some(normalize_to_string(&dst));
    }
    let base = Path::new(src_file).parent().unwrap_or(Path::new(root));
    let dst = normalize_path(base.join(uri));
    Some(normalize_to_string(&dst))
}

fn normalize_path(p: PathBuf) -> PathBuf {
    use std::path::Component::*;
    let mut stack: Vec<PathBuf> = Vec::new();
    for comp in p.components() {
        match comp {
            CurDir => {}
            ParentDir => {
                if stack.last().is_some() {
                    stack.pop();
                }
            }
            RootDir => stack.clear(),
            Normal(s) => stack.push(PathBuf::from(s)),
            Prefix(_) => {}
        }
    }
    let mut out = PathBuf::new();
    for seg in stack {
        out.push(seg);
    }
    out
}

fn normalize_to_string(p: &Path) -> String {
    normalize_path(p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Registry for `package:` name -> `<pkg-root>/lib` mapping,
/// built from scanning all pubspec.yaml and `path:` deps.
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

            // name -> lib
            if let Ok(meta) = serde_yml::from_str::<PubspecName>(&content) {
                if let Some(name) = meta.name {
                    map.entry(name).or_insert(base_dir.join("lib"));
                }
            }

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

fn collect_path_deps_into(
    base_dir: &Path,
    root: &serde_yml::Value,
    key: &str,
    out: &mut HashMap<String, PathBuf>,
) {
    let Some(deps) = root.get(key) else {
        return;
    };
    let Some(map) = deps.as_mapping() else {
        return;
    };

    for (k, v) in map {
        let Some(dep_name) = k.as_str() else {
            continue;
        };
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
