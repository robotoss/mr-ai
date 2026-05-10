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

## Integration tests (planned)

The S2 plan reserves [`testcontainers-rs`](https://docs.rs/testcontainers/)
for Postgres + Qdrant against real images, plus git fixture repos under
`tests/fixtures/`. The first integration suite ships in S5-B alongside
the E2E `webhook → review` smoke test. Today every integration-shaped
test that does not require Docker lives as a unit test in the relevant
crate.

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
