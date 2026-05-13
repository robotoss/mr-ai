# Testing

`cargo test --workspace --lib` is the source of truth. 60 unit tests run
in well under a second; CI runs them on every push.

## Layout

| Crate | Tests | What they cover |
| --- | --- | --- |
| `ai-llm-service` | 12 | Provider parsers, redaction, JSONL recorder. |
| `api` | 3 | Webhook payload extraction, body-hash determinism. |
| `code-indexer` | 7 | DartAnalyzer edge extraction (file node, defines, dedup, calls/type_uses, inheritance, async-boundary). |
| `domain` | 3 | EdgeKind round-trip, RetrievalConfig defaults, ChunkKind labels. |
| `git-context-engine` | 8 | Multi-repo aggregate fan-out, RetrievalPlan score floor / budget / expansion-min-hops, heuristic rerank ordering. |
| `persistence` | 4 | projects.toml parser (full example, defaults, error paths). |
| `project-code-store` | 4 | URL-to-path sanitisation across SSH/HTTPS forms. |
| `secrets` | 13 | Env / file backends; HMAC verifiers (GitLab token, GitHub sha256, Bitbucket sha256). |
| `services` | 4 | Retry helper (success after retry, max-attempts, classifier short-circuit, backoff doubling). |
| `worker` | 2 | Backoff doubles+caps, registry kind resolution. |

## Running

```bash
cargo test --workspace --lib                  # everything
cargo test -p secrets --lib                   # one crate
cargo test -p api --lib webhooks::common      # one module
cargo test -p api --lib -- --nocapture        # surface println!/tracing
```

Workspace-wide:

```bash
cargo build --workspace
cargo test --workspace --lib
```

## Adding a unit test

Inline `#[cfg(test)] mod tests { … }` blocks. Use `tokio::test` for async
helpers. Avoid env-var mutation across tests in the same module — pick a
unique key per case (see `secrets::tests::env_provider_*` for the
pattern). One env-mutation test in a parallel runner can crash any other
test that reads the same variable.

## Integration tests

[`persistence/tests/integration.rs`](../../persistence/tests/integration.rs)
boots a real Postgres via [`testcontainers-rs`](https://docs.rs/testcontainers/)
and exercises the persistence layer end-to-end. The suite is gated by
`#[ignore]` so the default `cargo test` path stays Docker-free. Run it
with:

```bash
cargo test --workspace --tests -- --ignored
```

(Docker daemon must be running.) Today the suite covers:

- migrations apply against a fresh database;
- `upsert_group` / `load_by_slug` round-trip with idempotent re-runs;
- graph node + edge upsert, neighbour lookup, edge-counts, `purge_repo`
  cascade;
- `webhook_events::record` dedup + status transitions;
- `mr_reviews` lifecycle (`pending → running → published`);
- `jobs` queue claim with `SKIP LOCKED` semantics;
- end-to-end webhook → enqueue → claim → complete → redelivery dedup
  flow (`webhook_to_queue_to_completion_flow`).

[`rag-base/tests/integration.rs`](../../rag-base/tests/integration.rs)
boots a real Qdrant via testcontainers and round-trips
`create_collection` → `upsert_points` → `search_points`. Same
`#[ignore]` gate; same one-line invocation.

[`api/tests/http_smoke.rs`](../../api/tests/http_smoke.rs) drives the
GitLab webhook handler through `axum::Router::oneshot` against the
testcontainers Postgres + the
[`ai_llm_service::test_support::dummy_gateway`](../../ai-llm-service/src/test_support.rs)
fixture. Asserts `202 Accepted` on first delivery, `webhook_events` +
`jobs` rows populated, replay returns `200 OK` with
`duplicate=true` and no extra job.

## Dart sidecar tests

[`dart_sidecar/test/analyzer_engine_test.dart`](../../dart_sidecar/test/analyzer_engine_test.dart)
covers the three AstVisitor passes (`dataFlowEdgesForUnit`,
`controlFlowEdgesForUnit`, `asyncBoundaryEdgesForUnit`) using
`package:analyzer`'s `parseString` helper — no Dart Analysis Server
bring-up required. Run with:

```bash
cd dart_sidecar
dart pub get
dart test
```

Coverage:

- `data_flow` — one edge per local-variable use (`total`, `i` in a
  counted loop), zero for branch-free bodies;
- `control_flow` — one edge per `if`/`for`/`while`/`do_while`/
  `switch`/`try`, zero for arrow functions;
- `async_boundary` — extracts the `MethodInvocation`/
  `SimpleIdentifier`/`PropertyAccess` callee from each `await`;
- owner chains (`<file>::<class>::<method>`) propagate into the emitted
  `from_fqn`.

## Smoke / manual checks

A short manual loop for verifying a fresh checkout boots:

```bash
cargo build --workspace
docker compose up -d postgres qdrant
just db-migrate
cargo run                                      # boots api + worker pool
curl -s http://localhost:8080/health/live      # → {"status":"ok"}
curl -s http://localhost:8080/health/detailed  # → component breakdown
```

The webhook test recipe in [Webhooks](webhooks.md#local-test-recipe)
covers replaying a fake provider event end-to-end.

## Common failure modes

- **`E0277` for `BTreeMap<NodeId, _>`**: needs `PartialOrd + Ord` on the
  ID type. The macro in `domain::ids` derives them — re-run `cargo build`
  to refresh derive expansion if you see this after a domain edit.
- **Flaky `secrets::tests::env_*`**: two tests racing on the same env
  var. Use a unique `SecretKey::Custom("…")` per test case.
- **`sqlx` type mismatch in tests**: `cargo sqlx prepare` regenerates
  `sqlx-data.json` so CI builds with `SQLX_OFFLINE=true`.

## Related docs

- [Installation](installation.md)
- [Observability](observability.md)
- [Database schema](../reference/database-schema.md)
