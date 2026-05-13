//! Default registry wiring for production. Builds a `Registry` containing
//! the three live job handlers against the supplied DB pool, the gateway
//! used for IngestMr's review build, a freshly-resolved `GitService`
//! wrapped in production port impls, and the shared `Qdrant` + `RagConfig`
//! that the context-engine's retrieval layer needs.

use std::sync::Arc;

use ai_llm_service::LlmGateway;
use project_code_store::{GitService, GitServiceConfig};
use qdrant_client::Qdrant;
use rag_base::structs::rag_base_config::RagConfig;
use sqlx::PgPool;

use crate::handlers::{IngestMrHandler, IngestPushHandler, ReindexHandler};
use crate::ports::{
    GitWorkspace, LlmGatewayPort, RealGitWorkspace, RealLlmGatewayPort, RealWorkspaceIndexer,
    WorkspaceIndexer,
};
use crate::{WorkerError, WorkerResult};

/// Wiring inputs for [`default_registry`]. Sprint C4 of 🅲 dropped
/// `project_name_legacy` — the worker now derives every per-job
/// tenant identity from `EnqueueOptions.project_id` and verifies it
/// against `remote_url` before any DB write.
///
/// `qdrant` and `rag_cfg` are shared with the API layer (both stored on
/// `AppState`) so per-request RAG retrieval doesn't reconnect.
#[derive(Clone)]
pub struct DefaultRegistryConfig {
    pub pool: PgPool,
    pub gateway: Arc<LlmGateway>,
    pub qdrant: Arc<Qdrant>,
    pub rag_cfg: Arc<RagConfig>,
    pub git_api_base: String,
}

/// Build the default registry for production. Wires every handler against
/// the supplied DB pool, the gateway used for IngestMr's review build,
/// the shared Qdrant client + RagConfig, and a freshly-resolved
/// `GitService` (env-driven config).
pub fn default_registry(cfg: DefaultRegistryConfig) -> WorkerResult<crate::Registry> {
    let DefaultRegistryConfig {
        pool,
        gateway,
        qdrant,
        rag_cfg,
        git_api_base,
    } = cfg;
    let git_service = GitService::new(GitServiceConfig::from_env())
        .map_err(|e| WorkerError::Handler("git_service_init".into(), Box::new(e)))?;
    // Sprint M2 of cross-repo MR review: `IngestMrHandler` takes the
    // concrete `GitService` directly so it can drive `build_for_mr`
    // for the overlay. `ReindexHandler` keeps the `GitWorkspace`
    // port abstraction it already uses; same underlying instance,
    // different facade.
    let mr_git = git_service.clone();
    let git: Arc<dyn GitWorkspace> = Arc::new(RealGitWorkspace::new(git_service));
    let gateway_port: Arc<dyn LlmGatewayPort> = Arc::new(RealLlmGatewayPort::new(gateway));
    let indexer: Arc<dyn WorkspaceIndexer> = Arc::new(RealWorkspaceIndexer::new());

    Ok(crate::Registry::builder()
        .register(IngestPushHandler::new(pool.clone(), git.clone()))
        .register(IngestMrHandler::new(
            pool.clone(),
            gateway_port.clone(),
            qdrant,
            rag_cfg,
            git_api_base,
            mr_git,
        ))
        .register(ReindexHandler::new(pool, git, gateway_port, indexer))
        .build())
}
