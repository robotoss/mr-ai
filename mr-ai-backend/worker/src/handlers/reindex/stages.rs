//! Typed stages for `ReindexHandler::handle_inner`. Each `impl` block
//! adds one stage so the linear pipeline in `mod.rs` stays a sequence
//! of `let resolved = self.resolve_repo(&parsed).await?` style calls.
//!
//! State threads through small structs (`RepoResolved`, `WorkspaceReady`,
//! `AnalysisResult`) so a stage's input/output is visible at the type
//! level and tests can construct it without touching the prior stages.

use std::path::PathBuf;

use code_indexer::{analyzer::AnalysisOutcome, CodeChunk};
use domain::{ProjectId, RepoId};
use persistence::graph_persist::{self, EdgeUpsert, NodeUpsert};
use persistence::repos::{index_state, projects};
use project_code_store::WorktreeHandle;
use serde_json::Value;
use tracing::info;

use crate::handlers::KIND_REINDEX;
use crate::{WorkerError, WorkerResult};

use super::auto_split::SubJobPlan;
use super::{ReindexHandler, ReindexPayload};

/// Identity of the repo + project resolved from the payload's `remote_url`.
#[derive(Debug, Clone, Copy)]
pub(super) struct RepoResolved {
    pub project_id: ProjectId,
    pub repo_id: RepoId,
}

/// Worktree + workspace path ready for indexing. `Drop` of the
/// `WorktreeHandle` removes the directory and prunes git.
#[derive(Debug)]
pub(super) struct WorkspaceReady {
    pub workspace: PathBuf,
    /// Held for its `Drop`: the worktree directory is removed and git
    /// is told to forget it when the pipeline ends or aborts. The
    /// field is otherwise read indirectly via [`Self::workspace`].
    #[allow(dead_code)]
    pub worktree: WorktreeHandle,
    #[allow(dead_code)] // surfaced for tracing/diagnostics in future stages
    pub job_tag: String,
    #[allow(dead_code)]
    pub ref_spec: String,
}

/// Outcome of the auto-split pre-flight. Either we proceed to a full
/// analyze pass, or we fan out one sub-job per top-level dir.
pub(super) enum SplitDecision {
    Proceed,
    FanOut(SubJobPlan),
}

/// What `analyze_workspace` produces. Owned `CodeChunk` + `AnalysisOutcome`
/// flow into `persist_graph` and `upsert_chunks` as separate slices.
pub(super) struct AnalysisResult {
    pub chunks: Vec<CodeChunk>,
    pub outcome: AnalysisOutcome,
}

impl ReindexHandler {
    pub(super) fn parse_payload(payload: Value) -> WorkerResult<ReindexPayload> {
        serde_json::from_value(payload).map_err(|e| WorkerError::BadPayload {
            kind: KIND_REINDEX.into(),
            msg: e.to_string(),
        })
    }

    pub(super) async fn resolve_repo(
        &self,
        parsed: &ReindexPayload,
    ) -> WorkerResult<RepoResolved> {
        let resolved = projects::find_repo_by_remote_url_lenient(&self.pool, &parsed.remote_url)
            .await
            .map_err(WorkerError::Persistence)?;
        let Some((project_id, repo_id)) = resolved else {
            return Err(WorkerError::BadPayload {
                kind: KIND_REINDEX.into(),
                msg: format!("unknown remote_url: {}", parsed.remote_url),
            });
        };
        Ok(RepoResolved {
            project_id,
            repo_id,
        })
    }

    pub(super) async fn open_workspace(
        &self,
        _resolved: &RepoResolved,
        parsed: &ReindexPayload,
    ) -> WorkerResult<WorkspaceReady> {
        // Resolve the ref to check out. Prefer head_sha; fall back to the
        // declared branch; default to FETCH_HEAD when neither is supplied.
        let ref_spec: String = parsed
            .head_sha
            .clone()
            .or(parsed.branch.clone())
            .unwrap_or_else(|| "FETCH_HEAD".into());
        let job_tag = format!("reindex-{}", uuid::Uuid::new_v4().simple());

        let worktree = self
            .git
            .create_worktree(&parsed.remote_url, &ref_spec, &job_tag)
            .await
            .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), e))?;

        let workspace: PathBuf =
            worktree.path().map(PathBuf::from).ok_or_else(|| {
                WorkerError::Handler(
                    KIND_REINDEX.into(),
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "worktree path missing",
                    )),
                )
            })?;

        Ok(WorkspaceReady {
            workspace,
            worktree,
            job_tag,
            ref_spec,
        })
    }

    #[tracing::instrument(name = "reindex.analyze_workspace", skip_all)]
    pub(super) async fn analyze_workspace(
        &self,
        ws: &WorkspaceReady,
        parsed: &ReindexPayload,
        _resolved: &RepoResolved,
    ) -> WorkerResult<AnalysisResult> {
        // Run the indexer + analyzer on a blocking pool — tree-sitter is
        // sync and walking 10⁵-file workspaces stalls the runtime
        // otherwise.
        let indexer = self.indexer.clone();
        let workspace_clone = ws.workspace.clone();
        let path_prefix_owned = parsed.path_prefix.clone();
        let analysis = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let chunks = indexer
                .index_filtered(&workspace_clone, false, path_prefix_owned.as_deref())
                .map_err(|e| e.to_string())?;

            let mut outcome = indexer.run_analyzer_fanout(&chunks);

            // Optional Dart Analyzer sidecar augmentation (S8). Failures
            // degrade the run to tree-sitter-only data instead of aborting.
            let dart_files: Vec<String> = {
                let mut seen = std::collections::HashSet::<&str>::new();
                let mut out = Vec::new();
                for c in chunks
                    .iter()
                    .filter(|c| matches!(c.language, code_indexer::LanguageKind::Dart))
                {
                    if seen.insert(c.file.as_str()) {
                        out.push(c.file.clone());
                    }
                }
                out
            };
            match indexer.augment_dart_sidecar(&mut outcome, &workspace_clone, dart_files) {
                Ok(true) => tracing::info!(
                    target = "worker.handler",
                    "Reindex: sidecar augmentation applied"
                ),
                Ok(false) => tracing::debug!(
                    target = "worker.handler",
                    "Reindex: sidecar disabled"
                ),
                Err(err) => tracing::warn!(
                    target = "worker.handler",
                    error = %err,
                    error.debug = ?err,
                    "Reindex: sidecar augmentation failed; continuing"
                ),
            }
            Ok((chunks, outcome))
        })
        .await
        .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;

        let (chunks, outcome) = analysis.map_err(|msg| {
            WorkerError::Handler(
                KIND_REINDEX.into(),
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg)),
            )
        })?;

        info!(
            target = "worker.handler",
            chunks = chunks.len(),
            nodes = outcome.nodes.len(),
            edges = outcome.edges.len(),
            coverage = ?outcome.coverage,
            "Reindex: analyzer finished"
        );
        Ok(AnalysisResult { chunks, outcome })
    }

    #[tracing::instrument(name = "reindex.persist_graph", skip_all)]
    pub(super) async fn persist_graph(
        &self,
        resolved: &RepoResolved,
        outcome: AnalysisOutcome,
    ) -> WorkerResult<()> {
        let nodes: Vec<NodeUpsert> = outcome
            .nodes
            .into_iter()
            .map(|n| NodeUpsert {
                fqn: n.fqn,
                kind: n.kind,
                file: n.file,
                symbol: n.symbol,
                language: n.language,
                content_sha256: n.content_sha256,
                span_start: n.span_start,
                span_end: n.span_end,
            })
            .collect();
        let edges: Vec<EdgeUpsert> = outcome
            .edges
            .into_iter()
            .map(|e| EdgeUpsert {
                from_fqn: e.from_fqn,
                to_fqn: e.to_fqn,
                edge_type: e.edge_type,
                weight: e.weight,
                meta: e.meta,
            })
            .collect();

        let persist = graph_persist::persist_graph(&self.pool, resolved.repo_id, &nodes, &edges)
            .await
            .map_err(WorkerError::Persistence)?;
        info!(
            target = "worker.handler",
            ?persist,
            "Reindex: graph persisted"
        );
        Ok(())
    }

    #[tracing::instrument(name = "reindex.upsert_chunks", skip_all, fields(chunks = chunks.len()))]
    pub(super) async fn upsert_chunks(
        &self,
        resolved: &RepoResolved,
        parsed: &ReindexPayload,
        chunks: &[CodeChunk],
    ) -> WorkerResult<()> {
        // Embedding pipeline: diff content_sha256 against what already
        // lives in Qdrant for this repo so unchanged chunks survive
        // without re-embedding. Failures map to WorkerError::Handler so
        // the job is retried via the existing backoff path.
        let rag_cfg = rag_base::structs::rag_base_config::RagConfig::from_env(None)
            .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;
        let qdrant_client = rag_base::vector_db::connect(&rag_cfg)
            .await
            .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;
        let repo_uuid: uuid::Uuid = resolved.repo_id.into();
        let project_uuid: uuid::Uuid = resolved.project_id.into();
        let repo_id_str = repo_uuid.simple().to_string();
        let project_id_str = project_uuid.simple().to_string();
        // Auto-split sub-jobs carry `path_prefix` so each pass only
        // sweeps orphans inside its own subtree — otherwise sub-job A
        // would delete the chunks sub-job B just upserted (and vice
        // versa). `None` keeps legacy whole-repo behaviour.
        let report = rag_base::upsert_repo_chunks(
            &qdrant_client,
            &rag_cfg,
            &self.gateway.concrete(),
            &repo_id_str,
            Some(&project_id_str),
            chunks,
            parsed.path_prefix.as_deref(),
        )
        .await
        .map_err(|e| WorkerError::Handler(KIND_REINDEX.into(), Box::new(e)))?;
        info!(
            target = "worker.handler",
            upserted = report.upserted,
            embedded = report.embedded,
            kept = report.kept,
            deleted = report.deleted,
            duration_ms = report.duration_ms,
            "Reindex: vector upsert finished"
        );
        Ok(())
    }

    pub(super) async fn mark_indexed(
        &self,
        resolved: &RepoResolved,
        parsed: &ReindexPayload,
    ) -> WorkerResult<()> {
        if let Some(sha) = parsed.head_sha.as_deref() {
            index_state::mark_indexed(&self.pool, resolved.repo_id, sha)
                .await
                .map_err(WorkerError::Persistence)?;
        }
        Ok(())
    }
}
