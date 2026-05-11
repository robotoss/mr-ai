# Getting Started

End-to-end local setup for a developer joining the project. Follow each
section in order; if one step fails the next ones won't work.

## Prerequisites

- **Rust stable** (matches `Cargo.toml` — currently edition 2024).
- **Docker + Docker Compose** for the local Ollama + Qdrant stack.
- **8–30 GB free disk** for cloned repos, JSONL artefacts, and embeddings.
- **SSH access** to your Git provider, or an HTTPS token.

## 1. Clone and bootstrap

```bash
git clone <this-repo>
cd mr-ai-backend
cp .env.example .env
```

Edit `.env`. The minimum fields needed for a smoke run on Ollama only:

```env
API_ADDRESS=0.0.0.0:8080
# Path is optional; default is `projects.toml` in the working directory.
PROJECTS_CONFIG=projects.toml

LLM_FAST_PROVIDER=ollama
LLM_FAST_MODEL=llama3
LLM_SMART_PROVIDER=ollama
LLM_SMART_MODEL=llama3
LLM_EMBED_PROVIDER=ollama
LLM_EMBED_MODEL=bge-m3

OLLAMA_URL=http://localhost:11434
QDRANT_URL=http://localhost:6334
LLM_PRICING_PATH=pricing.toml

# Required by the trigger route even for smoke runs.
GIT_API_BASE=https://gitlab.example.com/api/v4
GIT_TOKEN=fake-for-smoke-only
TRIGGER_SECRET=secret123
```

`projects.toml` must declare exactly one `[[project]]`:

```toml
[[project]]
slug = "demo"
name = "Demo"

[[project.repo]]
provider = "gitlab"
remote_url = "git@github.com:org/repo.git"
is_primary = true
```

Full list of variables: [Configuration](configuration.md).

## 2. Start the local stack

```bash
docker compose up -d
# Verifies Ollama at :11434 and Qdrant at :6334
```

Pull the models into Ollama once:

```bash
docker compose exec ollama ollama pull llama3
docker compose exec ollama ollama pull bge-m3
```

## 3. Build and run

```bash
cargo build --workspace
cargo run --release
```

You should see in the console:

```
LlmGateway initialised
fast.provider=ollama … smart.provider=ollama … embed.provider=ollama …
PriceTable loaded entries=N
LogConfig … logs/mr-ai.<date>
Server is listening on: 0.0.0.0:8080
```

A daily-rotated JSON log file appears at `logs/mr-ai.YYYY-MM-DD`.

## 4. Smoke test the routes

### Indexing

Operator routes require the `X-Admin-Token` header — it matches your
`TRIGGER_SECRET` env value (constant-time compare).

```bash
# Enqueue a Reindex job for every repo declared in projects.toml.
curl -X POST http://localhost:8080/admin/reindex_all \
  -H "X-Admin-Token: $TRIGGER_SECRET" \
  -d '{}'

# Or target a single repo.
curl -X POST http://localhost:8080/admin/reindex_repo \
  -H 'content-type: application/json' \
  -H "X-Admin-Token: $TRIGGER_SECRET" \
  -d '{"remote_url":"git@github.com:org/repo.git"}'
```

The worker pool will pick the jobs up, build a worktree, run the
per-language analyzers, persist the code graph in Postgres, and upsert
embeddings into Qdrant (S2 content-sha dedup). Watch the API logs for
the `Reindex: vector upsert finished` line.

### Retrieve

```bash
curl -X POST http://localhost:8080/retrieve \
  -H 'content-type: application/json' \
  -H "X-Admin-Token: $TRIGGER_SECRET" \
  -d '{"query":"user authentication middleware","top_k":8}' | jq
```

See [Retrieve API](../reference/retrieve-api.md) for the full request
shape (`repo_id`, `mr_iid`, `expand`, `kinds`, …) and the response
fields (`via`, `chunk_kind`, `overlay_meta`).

### Search

```bash
curl -X POST http://localhost:8080/search_vector_base \
  -H 'content-type: application/json' \
  -d '{"query":"user authentication middleware","k":10}'
```

### MR review

```bash
curl -X POST http://localhost:8080/trigger_git_mr \
  -H 'content-type: application/json' \
  -d '{"project_id":"team/repo","mr_iid":42,"secret":"secret123"}'
```

In the logs you'll see one structured `info!` line per LLM call carrying
`tier`, `provider`, `model`, token counts, `cost_usd`, `latency_ms`, and
`request_id`. See [Observability](observability.md) for details.

## 5. Run the test suite

```bash
cargo test -p ai-llm-service
```

Should print **32 passed, 0 failed**. These are hermetic and don't need
Ollama / Qdrant / network.

## Next steps

- Switch a tier to OpenAI or Bedrock — see [Configuration](configuration.md).
- Add a new provider — see [Add a new LLM Provider](add-llm-provider.md).
- Inspect cost analytics — see [Observability](observability.md).
