//! URI helpers for Dart.
//!
//! - [`resolve_relative`] handles `./`, `../`, and direct `*.dart` relative to the source file.
//! - [`DartPackageRegistry`] scans all `pubspec.yaml` files in a monorepo to build a `package:` map.
//!   It also reads path-based dependencies in `dependencies`, `dev_dependencies`, and `dependency_overrides`.

use serde::Deserialize;
use serde_yml::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, warn};
use walkdir::WalkDir;

/// Resolve a relative Dart import/export spec into an absolute path.
///
/// This handles only local paths (`./`, `../`, `*.dart`).
/// `package:` and `dart:` URIs are ignored (handled elsewhere).
///
/// # Example
/// ```rust
/// use std::path::Path;
/// use codegraph_prep::languages::dart::uri::resolve_relative;
///
/// let src = Path::new("/proj/lib/src/foo.dart");
/// let resolved = resolve_relative(src, "../bar.dart");
/// assert!(resolved.unwrap().ends_with("lib/bar.dart"));
/// ```
pub fn resolve_relative(src_file: &Path, spec: &str) -> Option<PathBuf> {
    if !(spec.starts_with("./") || spec.starts_with("../") || spec.ends_with(".dart")) {
        return None;
    }
    let base = src_file.parent().unwrap_or_else(|| Path::new(""));
    let candidate = base.join(spec);

    match dunce::canonicalize(&candidate) {
        Ok(p) => Some(p),
        Err(e) => {
            warn!(
                "Failed to canonicalize relative path {:?} from base {:?}: {}",
                spec, base, e
            );
            Some(candidate) // fallback to non-canonicalized path
        }
    }
}

/// Monorepo-wide registry mapping Dart `package:` URIs to filesystem paths.
///
/// It scans all `pubspec.yaml` files under a given root directory.
/// Each entry is mapped as: `package:foo/bar.dart` → `<pkg-root>/lib/bar.dart`.
#[derive(Debug, Default, Clone)]
pub struct DartPackageRegistry {
    packages: HashMap<String, PathBuf>, // name -> <dir>/lib
}

impl DartPackageRegistry {
    /// Build a package registry by scanning all `pubspec.yaml` under the given root directory.
    ///
    /// - Extracts the `name:` field from each pubspec.
    /// - Collects path-based dependencies (`dependencies`, `dev_dependencies`, `dependency_overrides`).
    ///
    /// # Errors
    /// Returns [`anyhow::Error`] if reading or parsing a `pubspec.yaml` fails.
    ///
    /// # Example
    /// ```no_run
    /// use std::path::Path;
    /// use codegraph_prep::languages::dart::uri::DartPackageRegistry;
    ///
    /// let root = Path::new("/path/to/monorepo");
    /// let registry = DartPackageRegistry::from_root(root).unwrap();
    /// let resolved = registry.resolve_package("package:foo/src/bar.dart");
    /// ```
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
            debug!("Parsing pubspec: {:?}", p);

            let content = match fs::read_to_string(p) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to read {:?}: {}", p, e);
                    continue;
                }
            };

            // Extract package name
            if let Ok(meta) = serde_yml::from_str::<PubspecName>(&content) {
                if let Some(name) = meta.name {
                    map.entry(name).or_insert(base_dir.join("lib"));
                }
            }

            // Path-based dependencies
            let val: Value = serde_yml::from_str(&content).unwrap_or(Value::Null);
            collect_path_deps_into(base_dir, &val, "dependencies", &mut map);
            collect_path_deps_into(base_dir, &val, "dev_dependencies", &mut map);
            collect_path_deps_into(base_dir, &val, "dependency_overrides", &mut map);
        }

        Ok(Self { packages: map })
    }

    /// Resolve a `package:` URI to its absolute path if known.
    ///
    /// # Example
    /// ```no_run
    /// use std::path::Path;
    /// use codegraph_prep::languages::dart::uri::DartPackageRegistry;
    ///
    /// let registry = DartPackageRegistry::from_root(Path::new("/repo")).unwrap();
    /// if let Some(abs) = registry.resolve_package("package:foo/bar.dart") {
    ///     println!("Resolved path: {:?}", abs);
    /// }
    /// ```
    pub fn resolve_package(&self, spec: &str) -> Option<PathBuf> {
        if !spec.starts_with("package:") {
            return None;
        }
        let rest = &spec["package:".len()..];
        let mut it = rest.splitn(2, '/');
        let pkg = it.next()?;
        let rel = it.next().unwrap_or("");
        let lib = self.packages.get(pkg)?;

        let resolved = lib.join(rel);
        debug!("Resolved package spec '{}' -> {:?}", spec, resolved);
        Some(resolved)
    }

    /// Number of registered packages.
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }
}

/// Collect path-based dependencies into the registry map.
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
                    debug!("Found path-based dep: {} -> {:?}", dep_name, dir);
                    out.entry(dep_name.to_string()).or_insert(dir.join("lib"));
                }
            }
        }
    }
}
