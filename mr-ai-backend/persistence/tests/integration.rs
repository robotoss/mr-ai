//! Integration tests against a real Postgres instance via testcontainers.
//!
//! Marked `#[ignore]` so the default `cargo test` path stays Docker-free —
//! run with `cargo test --workspace --tests -- --ignored` (and Docker
//! running) to exercise them. CI flips that switch in the
//! `--features integration` job once it ships in S8-B.

#![allow(clippy::needless_return)]

use domain::{
    EdgeKind, GraphEdge, GraphNode, NodeKind, NodeSpan, ProjectGroup, ProjectId, ProjectRepo,
    ProviderKind, RepoDependency, RepoId,
};
use persistence::repos::{graph, projects};
use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres as PgImage;

async fn boot_pool() -> (PgPool, testcontainers::ContainerAsync<PgImage>) {
    let container = PgImage::default()
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container port");
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let pool = persistence::init_pool(&persistence::PoolConfig {
        url,
        max_connections: 4,
        acquire_timeout: std::time::Duration::from_secs(10),
    })
    .await
    .expect("connect");
    persistence::run_migrations(&pool)
        .await
        .expect("migrations apply");
    (pool, container)
}

fn sample_group() -> ProjectGroup {
    let project_id = ProjectId::new();
    let primary = ProjectRepo {
        id: RepoId::new(),
        project_id,
        provider: ProviderKind::Gitlab,
        remote_url: "git@gitlab.com:org/app.git".into(),
        default_branch: "main".into(),
        is_primary: true,
    };
    let shared = ProjectRepo {
        id: RepoId::new(),
        project_id,
        provider: ProviderKind::Gitlab,
        remote_url: "git@gitlab.com:org/shared.git".into(),
        default_branch: "main".into(),
        is_primary: false,
    };
    ProjectGroup {
        id: project_id,
        slug: "monorepo".into(),
        name: "Monorepo".into(),
        repos: vec![primary.clone(), shared.clone()],
        dependencies: vec![RepoDependency {
            from_repo: primary.id,
            to_repo: shared.id,
            kind: "manual".into(),
        }],
    }
}

#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn migrations_apply_cleanly() {
    let (pool, _container) = boot_pool().await;
    // Smoke: SELECT 1 from a known migrated table.
    let count: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM projects")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn project_group_upsert_round_trip() {
    let (pool, _container) = boot_pool().await;
    let group = sample_group();
    projects::upsert_group(&pool, &group).await.unwrap();

    // Re-running with the same content should be idempotent.
    projects::upsert_group(&pool, &group).await.unwrap();

    let loaded = projects::load_by_slug(&pool, "monorepo").await.unwrap();
    let loaded = loaded.expect("group present");
    assert_eq!(loaded.repos.len(), 2);
    assert_eq!(loaded.dependencies.len(), 1);
    assert!(loaded.repos.iter().any(|r| r.is_primary));
}

#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn graph_persist_round_trip() {
    let (pool, _container) = boot_pool().await;
    let group = sample_group();
    projects::upsert_group(&pool, &group).await.unwrap();
    let primary = group
        .repos
        .iter()
        .find(|r| r.is_primary)
        .unwrap()
        .id;

    let file_node = GraphNode {
        id: None,
        repo_id: primary,
        fqn: "lib/main.dart".into(),
        kind: NodeKind::File,
        file: "lib/main.dart".into(),
        symbol: "main.dart".into(),
        language: "dart".into(),
        content_sha256: None,
        span: None,
    };
    let class_node = GraphNode {
        id: None,
        repo_id: primary,
        fqn: "lib/main.dart::App".into(),
        kind: NodeKind::Class,
        file: "lib/main.dart".into(),
        symbol: "App".into(),
        language: "dart".into(),
        content_sha256: Some("aa".into()),
        span: Some(NodeSpan { start: 0, end: 10 }),
    };

    let file_id = graph::upsert_node(&pool, &file_node).await.unwrap();
    let class_id = graph::upsert_node(&pool, &class_node).await.unwrap();
    graph::upsert_edge(
        &pool,
        &GraphEdge {
            from: file_id,
            to: class_id,
            edge_type: EdgeKind::Defines,
            weight: 1.0,
            meta: None,
        },
    )
    .await
    .unwrap();

    // Idempotent re-upsert of the same identities.
    let class_id_2 = graph::upsert_node(&pool, &class_node).await.unwrap();
    assert_eq!(class_id, class_id_2);

    let neighbours = graph::neighbours(&pool, file_id, Some(&EdgeKind::Defines))
        .await
        .unwrap();
    assert_eq!(neighbours, vec![class_id]);

    let counts = graph::edge_counts_by_type(&pool).await.unwrap();
    assert!(counts.iter().any(|(t, n)| t == "defines" && *n >= 1));

    // Cascade: dropping the repo nukes the graph.
    graph::purge_repo(&pool, primary).await.unwrap();
    let neighbours_after = graph::neighbours(&pool, file_id, None).await.unwrap();
    assert!(neighbours_after.is_empty());
}

#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn jobs_claim_skip_locked_round_trip() {
    use persistence::repos::jobs;
    let (pool, _container) = boot_pool().await;
    // No project_id needed for the queue smoke.
    let payload = serde_json::json!({"hello": "world"});
    let id = jobs::enqueue(&pool, "TestKind", &payload, jobs::EnqueueOptions::default())
        .await
        .unwrap();
    let claimed = jobs::claim_next(&pool, "worker-test")
        .await
        .unwrap()
        .expect("claim returns the queued job");
    assert_eq!(claimed.id, id);
    jobs::complete(&pool, id).await.unwrap();
    let again = jobs::claim_next(&pool, "worker-test").await.unwrap();
    assert!(again.is_none());
}

#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn webhook_event_dedup_and_status_transitions() {
    use persistence::repos::webhook_events::{self, RecordOutcome, WebhookRecord};
    let (pool, _container) = boot_pool().await;
    let rec = WebhookRecord {
        provider: ProviderKind::Gitlab,
        event_id: "test-event-1".into(),
        event_kind: "merge_request".into(),
        payload_hash: vec![1, 2, 3],
        payload: serde_json::json!({"object_kind": "merge_request"}),
    };
    let first = webhook_events::record(&pool, &rec).await.unwrap();
    assert_eq!(first.outcome, RecordOutcome::Inserted);
    let dup = webhook_events::record(&pool, &rec).await.unwrap();
    assert_eq!(dup.outcome, RecordOutcome::Duplicate);
    assert_eq!(dup.id, first.id);

    webhook_events::mark_enqueued(&pool, first.id).await.unwrap();
    let status: (String,) =
        sqlx::query_as("SELECT status FROM webhook_events WHERE id = $1")
            .bind(uuid::Uuid::from(first.id))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status.0, "enqueued");

    webhook_events::mark_rejected(&pool, first.id).await.unwrap();
    let status: (String,) =
        sqlx::query_as("SELECT status FROM webhook_events WHERE id = $1")
            .bind(uuid::Uuid::from(first.id))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status.0, "rejected");
}

/// Full webhook → enqueue → claim → complete flow exercised against a
/// real Postgres. Substitutes for an HTTP-level E2E smoke until the
/// review pipeline grows a mockable LlmGateway fixture.
#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn webhook_to_queue_to_completion_flow() {
    use persistence::repos::jobs::{self, EnqueueOptions};
    use persistence::repos::webhook_events::{self, RecordOutcome, WebhookRecord};

    let (pool, _container) = boot_pool().await;

    // 1. Register the project group so the webhook resolves to a repo.
    let group = sample_group();
    projects::upsert_group(&pool, &group).await.unwrap();
    let primary = group.repos.iter().find(|r| r.is_primary).unwrap();

    // 2. Webhook arrives — record the event.
    let payload = serde_json::json!({
        "object_kind": "merge_request",
        "project": { "git_ssh_url": primary.remote_url },
        "object_attributes": {
            "iid": 99,
            "source_branch": "feat",
            "target_branch": "main",
            "last_commit": { "id": "deadbeef" }
        }
    });
    let event = webhook_events::record(
        &pool,
        &WebhookRecord {
            provider: ProviderKind::Gitlab,
            event_id: "ev-1".into(),
            event_kind: "merge_request".into(),
            payload_hash: vec![0u8; 32],
            payload: payload.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(event.outcome, RecordOutcome::Inserted);

    // 3. Enqueue the IngestMr job that the webhook handler would create.
    let job_id = jobs::enqueue(
        &pool,
        "IngestMr",
        &serde_json::json!({
            "provider": "gitlab",
            "remote_url": primary.remote_url,
            "mr_iid": "99"
        }),
        EnqueueOptions {
            project_id: Some(primary.project_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    webhook_events::mark_enqueued(&pool, event.id).await.unwrap();

    // 4. Worker claims and completes the job.
    let claimed = jobs::claim_next(&pool, "smoke-worker")
        .await
        .unwrap()
        .expect("queued job available");
    assert_eq!(claimed.id, job_id);
    assert_eq!(claimed.kind, "IngestMr");
    jobs::complete(&pool, job_id).await.unwrap();

    // 5. Idempotency: redelivery of the same webhook does not enqueue again.
    let dup = webhook_events::record(
        &pool,
        &WebhookRecord {
            provider: ProviderKind::Gitlab,
            event_id: "ev-1".into(),
            event_kind: "merge_request".into(),
            payload_hash: vec![0u8; 32],
            payload,
        },
    )
    .await
    .unwrap();
    assert_eq!(dup.outcome, RecordOutcome::Duplicate);
    assert_eq!(dup.id, event.id);

    // 6. Final ledger: one event, one queue entry, both terminal.
    let event_status: (String,) =
        sqlx::query_as("SELECT status FROM webhook_events WHERE id = $1")
            .bind(uuid::Uuid::from(event.id))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(event_status.0, "enqueued");
    let job_status: (String,) =
        sqlx::query_as("SELECT status FROM jobs WHERE id = $1")
            .bind(uuid::Uuid::from(job_id))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(job_status.0, "done");
}

#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn mr_reviews_lifecycle() {
    use persistence::repos::mr_reviews;
    let (pool, _container) = boot_pool().await;
    let group = sample_group();
    projects::upsert_group(&pool, &group).await.unwrap();
    let primary = group.repos.iter().find(|r| r.is_primary).unwrap();
    let mr = domain::MrId::new("42");
    let id = mr_reviews::upsert_pending(
        &pool,
        primary.project_id,
        primary.id,
        &mr,
        &serde_json::json!({"stage": "received"}),
    )
    .await
    .unwrap();
    mr_reviews::mark_running(&pool, id).await.unwrap();
    mr_reviews::finish(&pool, id, "published", &serde_json::json!({"final": true}))
        .await
        .unwrap();
    let row: (String,) = sqlx::query_as("SELECT status FROM mr_reviews WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.0, "published");
}

// =============================================================
// Sprint C5 (🅲): tenant isolation tests against real Postgres.
// =============================================================

/// `with_tenant` scopes SELECT to the supplied tenant via SET LOCAL.
/// We seed two tenants' MR reviews, run a query under tenant A's
/// scope, and verify only A's rows are visible. RLS policies from
/// migration 0015 do the filtering — without RLS this test would
/// return both rows.
#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn with_tenant_scopes_select_to_one_project() {
    use persistence::repos::mr_reviews;
    let (pool, _container) = boot_pool().await;

    // Seed two tenants with one MR review each.
    let group_a = sample_group();
    let group_b = sample_group();
    projects::upsert_group(&pool, &group_a).await.unwrap();
    projects::upsert_group(&pool, &group_b).await.unwrap();
    let repo_a = group_a.repos.iter().find(|r| r.is_primary).unwrap();
    let repo_b = group_b.repos.iter().find(|r| r.is_primary).unwrap();
    let mr_a = mr_reviews::upsert_pending(
        &pool,
        repo_a.project_id,
        repo_a.id,
        &domain::MrId::new("1"),
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    let mr_b = mr_reviews::upsert_pending(
        &pool,
        repo_b.project_id,
        repo_b.id,
        &domain::MrId::new("2"),
        &serde_json::json!({}),
    )
    .await
    .unwrap();

    // Pre-FORCE: the table owner bypasses RLS, so both reviews are
    // visible from a raw query. This is the current production
    // posture; FORCE will close the bypass once every callsite is
    // on `with_tenant` (see `persistence/scripts/force_rls.sql`).
    let all: Vec<(uuid::Uuid,)> = sqlx::query_as("SELECT id FROM mr_reviews ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);

    // Temporarily FORCE RLS on `mr_reviews` to validate the policy.
    sqlx::query("ALTER TABLE mr_reviews FORCE ROW LEVEL SECURITY")
        .execute(&pool)
        .await
        .unwrap();

    // Scoped read via with_tenant: only A's review.
    let scope_a = domain::AuthorizedScope::from_project_id(repo_a.project_id);
    let visible_a = persistence::with_tenant(&pool, &scope_a, |tx| {
        Box::pin(async move {
            let rows: Vec<(uuid::Uuid,)> =
                sqlx::query_as("SELECT id FROM mr_reviews ORDER BY id")
                    .fetch_all(&mut **tx)
                    .await?;
            Ok::<_, persistence::PersistenceError>(rows)
        })
    })
    .await
    .unwrap();
    let visible_a_uuids: Vec<uuid::Uuid> = visible_a.iter().map(|(id,)| *id).collect();
    assert!(visible_a_uuids.contains(&mr_a), "tenant A should see its own review");
    assert!(
        !visible_a_uuids.contains(&mr_b),
        "tenant A must NOT see tenant B's review"
    );

    // Symmetry — B sees its own only.
    let scope_b = domain::AuthorizedScope::from_project_id(repo_b.project_id);
    let visible_b = persistence::with_tenant(&pool, &scope_b, |tx| {
        Box::pin(async move {
            let rows: Vec<(uuid::Uuid,)> =
                sqlx::query_as("SELECT id FROM mr_reviews ORDER BY id")
                    .fetch_all(&mut **tx)
                    .await?;
            Ok::<_, persistence::PersistenceError>(rows)
        })
    })
    .await
    .unwrap();
    let visible_b_uuids: Vec<uuid::Uuid> = visible_b.iter().map(|(id,)| *id).collect();
    assert!(visible_b_uuids.contains(&mr_b));
    assert!(!visible_b_uuids.contains(&mr_a));

    // Unscoped tx still sees zero rows under FORCE — the helper
    // never calls SET LOCAL, so `current_setting(..., true)` is NULL.
    let unscoped: Vec<(uuid::Uuid,)> =
        persistence::with_unscoped_tx::<_, Vec<(uuid::Uuid,)>>(&pool, |tx| {
            Box::pin(async move {
                let rows: Vec<(uuid::Uuid,)> =
                    sqlx::query_as("SELECT id FROM mr_reviews ORDER BY id")
                        .fetch_all(&mut **tx)
                        .await?;
                Ok(rows)
            })
        })
        .await
        .unwrap();
    assert_eq!(
        unscoped.len(),
        0,
        "FORCE RLS + missing SET LOCAL must see zero rows"
    );

    // Restore the table's default ENABLE state so subsequent tests
    // running in the same container aren't surprised.
    sqlx::query("ALTER TABLE mr_reviews NO FORCE ROW LEVEL SECURITY")
        .execute(&pool)
        .await
        .unwrap();
}

/// Transitive RLS via FK chain: `mr_review_hypotheses` policy reads
/// the parent `mr_reviews.project_id`. Insert two hypotheses across
/// tenants, FORCE the table, and verify each tenant only sees its
/// own row through `with_tenant`.
#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn rls_transitive_policy_scopes_mr_review_hypotheses() {
    use persistence::repos::{mr_review_hypotheses, mr_reviews};
    let (pool, _container) = boot_pool().await;

    let group_a = sample_group();
    let group_b = sample_group();
    projects::upsert_group(&pool, &group_a).await.unwrap();
    projects::upsert_group(&pool, &group_b).await.unwrap();
    let repo_a = group_a.repos.iter().find(|r| r.is_primary).unwrap();
    let repo_b = group_b.repos.iter().find(|r| r.is_primary).unwrap();
    let mr_a_id = mr_reviews::upsert_pending(
        &pool,
        repo_a.project_id,
        repo_a.id,
        &domain::MrId::new("1"),
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    let mr_b_id = mr_reviews::upsert_pending(
        &pool,
        repo_b.project_id,
        repo_b.id,
        &domain::MrId::new("2"),
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    let row_a = mr_review_hypotheses::HypothesisRow {
        review_id: mr_a_id,
        hypothesis_id: "H1".into(),
        priority: 0,
        tier_used: "smart".into(),
        status: "succeeded".into(),
        llm_response: None,
        latency_ms: Some(1),
        cost_usd: Some(0.001),
        created_at: chrono::Utc::now(),
    };
    let row_b = mr_review_hypotheses::HypothesisRow {
        review_id: mr_b_id,
        ..row_a.clone()
    };
    mr_review_hypotheses::insert(&pool, &row_a).await.unwrap();
    mr_review_hypotheses::insert(&pool, &row_b).await.unwrap();

    sqlx::query("ALTER TABLE mr_review_hypotheses FORCE ROW LEVEL SECURITY")
        .execute(&pool)
        .await
        .unwrap();

    let scope_a = domain::AuthorizedScope::from_project_id(repo_a.project_id);
    let visible_a = persistence::with_tenant(&pool, &scope_a, |tx| {
        Box::pin(async move {
            let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
                "SELECT review_id FROM mr_review_hypotheses ORDER BY review_id",
            )
            .fetch_all(&mut **tx)
            .await?;
            Ok::<_, persistence::PersistenceError>(rows)
        })
    })
    .await
    .unwrap();
    let ids: Vec<uuid::Uuid> = visible_a.iter().map(|(id,)| *id).collect();
    assert!(ids.contains(&mr_a_id));
    assert!(
        !ids.contains(&mr_b_id),
        "transitive RLS must not leak tenant B's hypothesis"
    );

    sqlx::query("ALTER TABLE mr_review_hypotheses NO FORCE ROW LEVEL SECURITY")
        .execute(&pool)
        .await
        .unwrap();
}
