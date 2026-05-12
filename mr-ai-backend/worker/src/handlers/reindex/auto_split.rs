//! S9 auto-split: when a workspace exceeds `REINDEX_SPLIT_FILES`, the
//! parent `Reindex` job fans out one sub-job per top-level directory.
//! All sub-jobs are enqueued inside a single transaction so a partial
//! failure can't leave the queue with half the work.

use std::path::PathBuf;

use persistence::repos::jobs::{self, EnqueueOptions};
use serde_json::json;
use tracing::info;

use crate::handlers::KIND_REINDEX;
use crate::{WorkerError, WorkerResult};

use super::stages::{RepoResolved, SplitDecision, WorkspaceReady};
use super::{ReindexHandler, ReindexPayload};

/// Concrete fan-out plan handed to [`ReindexHandler::dispatch_subjobs`].
/// Pure data, built by [`plan_split`] without touching the DB.
pub(super) struct SubJobPlan {
    pub dirs: Vec<String>,
    #[allow(dead_code)]
    pub file_count: usize,
}

/// File-count threshold above which the parent `Reindex` job fans
/// out one sub-job per top-level directory. Env knob:
/// `REINDEX_SPLIT_FILES` (default 5000). `0` disables auto-split.
fn reindex_split_threshold() -> usize {
    std::env::var("REINDEX_SPLIT_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5000)
}

/// Distinct top-level directories that appear among the indexed
/// files, plus the [`code_indexer::ROOT_BUCKET_PREFIX`] sentinel when
/// any file lives directly at the workspace root. The S9 auto-split
/// branch consumes this list so a Cargo-shaped workspace with `src/`
/// + a handful of root `*.rs` files still gets every file indexed.
pub(crate) fn top_level_dirs(
    workspace: &std::path::Path,
    files: &[std::path::PathBuf],
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut has_root_files = false;
    for f in files {
        let Ok(rel) = f.strip_prefix(workspace) else {
            continue;
        };
        let mut comps = rel.components();
        let Some(first) = comps.next() else { continue };
        if comps.next().is_some() {
            // Real top-level directory.
            out.insert(first.as_os_str().to_string_lossy().into_owned());
        } else {
            // Single component → file at the workspace root.
            has_root_files = true;
        }
    }
    let mut result: Vec<String> = out.into_iter().collect();
    if has_root_files {
        // The trailing `/` is added by the caller when building the
        // `path_prefix`; we emit a directory name only.
        result.push(
            code_indexer::ROOT_BUCKET_PREFIX
                .trim_end_matches('/')
                .to_owned(),
        );
    }
    result
}

/// Pure planner: decide whether to fan out from a workspace's file
/// inventory + payload. Sub-jobs (`path_prefix` already set) never
/// re-split; a single-dir workspace never splits (one sub-job is the
/// parent, byte-identical).
pub(crate) fn plan_split_pure(
    workspace: &std::path::Path,
    files: &[PathBuf],
    has_path_prefix: bool,
    split_threshold: usize,
) -> SplitDecision {
    if has_path_prefix || split_threshold == 0 {
        return SplitDecision::Proceed;
    }
    if files.len() <= split_threshold {
        return SplitDecision::Proceed;
    }
    let dirs = top_level_dirs(workspace, files);
    if dirs.len() <= 1 {
        return SplitDecision::Proceed;
    }
    SplitDecision::FanOut(SubJobPlan {
        dirs,
        file_count: files.len(),
    })
}

impl ReindexHandler {
    /// S9 auto-split pre-flight. Lists the workspace via the
    /// `WorkspaceIndexer` port (so tests can feed a fixture set) and
    /// delegates to [`plan_split_pure`] for the decision.
    pub(super) async fn plan_split(
        &self,
        parsed: &ReindexPayload,
        ws: &WorkspaceReady,
    ) -> WorkerResult<SplitDecision> {
        let split_threshold = reindex_split_threshold();
        if parsed.path_prefix.is_some() || split_threshold == 0 {
            return Ok(SplitDecision::Proceed);
        }
        let indexer_for_count = self.indexer.clone();
        let workspace_for_count = ws.workspace.clone();
        let file_list = tokio::task::spawn_blocking(move || {
            indexer_for_count.list_files(&workspace_for_count)
        })
        .await
        .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;
        Ok(plan_split_pure(
            &ws.workspace,
            &file_list,
            parsed.path_prefix.is_some(),
            split_threshold,
        ))
    }

    /// Enqueue every sub-job in one transaction. Without that, a
    /// partial failure (e.g. job 5 of 10 fails to insert) would leave
    /// the queue with 4 orphan sub-jobs *and* the parent retries to
    /// enqueue another batch, doubling the work indefinitely.
    pub(super) async fn dispatch_subjobs(
        &self,
        plan: SubJobPlan,
        resolved: &RepoResolved,
        parsed: &ReindexPayload,
    ) -> WorkerResult<()> {
        info!(
            target = "worker.handler",
            files = plan.file_count,
            dirs = plan.dirs.len(),
            "Reindex: file count above REINDEX_SPLIT_FILES; fanning out per-directory"
        );
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| WorkerError::Persistence(e.into()))?;
        for dir in &plan.dirs {
            let payload = json!({
                "remote_url": parsed.remote_url,
                "branch": parsed.branch,
                "head_sha": parsed.head_sha,
                "path_prefix": format!("{dir}/"),
            });
            jobs::enqueue_in_tx(
                &mut tx,
                KIND_REINDEX,
                &payload,
                EnqueueOptions {
                    project_id: Some(resolved.project_id),
                    ..Default::default()
                },
            )
            .await
            .map_err(WorkerError::Persistence)?;
        }
        tx.commit()
            .await
            .map_err(|e| WorkerError::Persistence(e.into()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{plan_split_pure, top_level_dirs, SplitDecision};

    fn files(workspace: &Path, rels: &[&str]) -> Vec<PathBuf> {
        rels.iter().map(|r| workspace.join(r)).collect()
    }

    #[test]
    fn top_level_dirs_includes_root_bucket_when_root_files_present() {
        let workspace = Path::new("/ws");
        let files = files(
            workspace,
            &["src/main.rs", "Cargo.toml", "README.md", "src/lib.rs"],
        );
        let mut dirs = top_level_dirs(workspace, &files);
        dirs.sort();
        let root = code_indexer::ROOT_BUCKET_PREFIX
            .trim_end_matches('/')
            .to_owned();
        assert!(dirs.contains(&root), "expected ROOT_BUCKET_PREFIX in {dirs:?}");
        assert!(dirs.contains(&"src".to_string()));
        assert_eq!(
            dirs.len(),
            2,
            "duplicate top-level dirs should be collapsed: {dirs:?}"
        );
    }

    #[test]
    fn top_level_dirs_strips_workspace_prefix_and_dedupes() {
        let workspace = Path::new("/ws");
        let files = files(
            workspace,
            &[
                "src/a.rs",
                "src/b.rs",
                "src/inner/c.rs",
                "tests/t1.rs",
                "tests/t2.rs",
            ],
        );
        let dirs = top_level_dirs(workspace, &files);
        // All three top-level entries are dirs (no root files).
        let mut sorted = dirs.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["src".to_string(), "tests".to_string()]);
    }

    #[test]
    fn plan_split_returns_proceed_below_threshold() {
        let workspace = Path::new("/ws");
        let files = files(workspace, &["src/a.rs", "src/b.rs"]);
        let decision = plan_split_pure(workspace, &files, /*has_prefix*/ false, /*threshold*/ 10);
        assert!(matches!(decision, SplitDecision::Proceed));
    }

    #[test]
    fn plan_split_returns_fan_out_above_threshold_with_multi_dirs() {
        let workspace = Path::new("/ws");
        let files = files(
            workspace,
            &[
                "src/a.rs",
                "src/b.rs",
                "tests/t.rs",
                "docs/x.md",
                "Cargo.toml", // root file → ROOT_BUCKET_PREFIX
            ],
        );
        let decision = plan_split_pure(workspace, &files, false, /*threshold*/ 2);
        match decision {
            SplitDecision::FanOut(plan) => {
                assert_eq!(plan.file_count, 5);
                assert!(plan.dirs.len() >= 2);
            }
            SplitDecision::Proceed => panic!("expected FanOut above threshold with multi dirs"),
        }
    }

    #[test]
    fn plan_split_returns_proceed_when_path_prefix_set() {
        let workspace = Path::new("/ws");
        let files = files(
            workspace,
            &["src/a.rs", "tests/t.rs", "docs/x.md", "Cargo.toml"],
        );
        // Sub-jobs (already-prefixed) never recurse — the parent fan-out
        // has already decomposed the workspace into per-dir slices.
        let decision = plan_split_pure(workspace, &files, /*has_prefix*/ true, 1);
        assert!(matches!(decision, SplitDecision::Proceed));
    }

    #[test]
    fn plan_split_returns_proceed_when_single_dir_above_threshold() {
        let workspace = Path::new("/ws");
        // Many files but all under `src/` → splitting yields one bucket,
        // which is byte-identical to running the parent. Skip the fan-out.
        let files = files(
            workspace,
            &["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs"],
        );
        let decision = plan_split_pure(workspace, &files, false, /*threshold*/ 2);
        assert!(matches!(decision, SplitDecision::Proceed));
    }

    #[test]
    fn plan_split_returns_proceed_when_threshold_is_zero() {
        let workspace = Path::new("/ws");
        let files = files(workspace, &["src/a.rs", "tests/t.rs"]);
        // `REINDEX_SPLIT_FILES=0` disables auto-split entirely.
        let decision = plan_split_pure(workspace, &files, false, 0);
        assert!(matches!(decision, SplitDecision::Proceed));
    }
}
