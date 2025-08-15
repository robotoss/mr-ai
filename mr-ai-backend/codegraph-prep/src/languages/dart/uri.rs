//! URI helpers for Dart.
//!
//! - `resolve_relative`: handles `./`, `../`, and direct `*.dart` relative to the source file.
//! - `DartPackageRegistry`: scans all `pubspec.yaml` in a monorepo to build a `package:` map.
//!   It also reads path-based deps in `dependencies`, `dev_dependencies`, and `dependency_overrides`.

use serde::Deserialize;
use serde_yml::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

pub fn resolve_relative(src_file: &Path, spec: &str) -> Option<PathBuf> {
    if !(spec.starts_with("./") || spec.starts_with("../") || spec.ends_with(".dart")) {
        return None;
    }
    let base = src_file.parent().unwrap_or_else(|| Path::new(""));
    let p = base.join(spec);
    Some(dunce::canonicalize(&p).unwrap_or(p))
}

/// Monorepo-wide package registry for `package:` URI resolution.
///
/// `package:foo/bar.dart` → `<pkg-root>/lib/bar.dart`
#[derive(Debug, Default, Clone)]
pub struct DartPackageRegistry {
    packages: HashMap<String, PathBuf>, // name -> <dir>/lib
}

impl DartPackageRegistry {
    /// Build registry by scanning all `pubspec.yaml` under `root`.
    pub fn from_root(root: &Path) -> anyhow::Result<Self> {
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

            let base_dir = p.parent().unwrap_or(root);
            let content = fs::read_to_string(p)?;
            // name:
            if let Ok(meta) = serde_yml::from_str::<PubspecName>(&content) {
                if let Some(name) = meta.name {
                    map.entry(name).or_insert(base_dir.join("lib"));
                }
            }

            // path-based deps
            let val: Value = serde_yml::from_str(&content).unwrap_or(Value::Null);
            collect_path_deps_into(base_dir, &val, "dependencies", &mut map);
            collect_path_deps_into(base_dir, &val, "dev_dependencies", &mut map);
            collect_path_deps_into(base_dir, &val, "dependency_overrides", &mut map);
        }

        Ok(Self { packages: map })
    }

    /// Given `package:foo/path.dart`, return absolute path `<lib-of-foo>/path.dart` if known.
    pub fn resolve_package(&self, spec: &str) -> Option<PathBuf> {
        if !spec.starts_with("package:") {
            return None;
        }
        let rest = &spec["package:".len()..];
        let mut it = rest.splitn(2, '/');
        let pkg = it.next()?;
        let rel = it.next().unwrap_or("");
        let lib = self.packages.get(pkg)?;
        Some(lib.join(rel))
    }
}

fn collect_path_deps_into(
    base_dir: &Path,
    root: &Value,
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
            if let Some(path_val) = dep_map.get(&Value::from("path")) {
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
