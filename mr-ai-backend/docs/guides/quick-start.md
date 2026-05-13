# Quick Start

From zero to a working MR review in **~30 minutes**. This guide walks
you through indexing a Flutter repo, running your first review, then
expanding into a Flutter monorepo to see cross-repo context in action.

By the end you'll have:
- a local stack (Postgres, Qdrant, Ollama) running in Docker
- one Flutter repository indexed into the vector store
- a real MR review bundle generated against a real diff
- (advanced) a webhook listening for live MRs, posting comments back

## TL;DR

1. **Install prerequisites** — Docker, Rust toolchain, `just`, `ngrok` (optional). _~5 min_
2. **Start infra** — `docker compose up -d` brings Postgres + Qdrant. _~2 min_
3. **Install Ollama + pull models** — `bge-m3` for embeddings, `qwen2.5-coder:7b` for review. _~5–10 min_
4. **Configure** — copy `.env` + create `projects.toml` for one Flutter repo. _~3 min_
5. **Migrate DB + run** — `just db-migrate && cargo run --release`. _~5 min on first build_
6. **First index** — `POST /admin/reindex_all`. _~3 min depending on repo size_
7. **First review** — `POST /trigger_git_mr` with a real MR IID, inspect `mr_reviews.bundle`. _~2 min_
8. _(Advanced)_ Expand to a monorepo + enable webhooks + publish comments. _~15 min_

```mermaid
graph LR
    Dev[Developer] -->|MR webhook| API[mr-ai-backend API :8080]
    API -->|jobs| PG[(Postgres :5432)]
    Worker[Worker pool] -->|claim job| PG
    Worker -->|index / review| LLM[Ollama :11434]
    Worker -->|vectors| QD[(Qdrant :6334)]
    Worker -->|publish| Git[GitLab/GitHub]
```

---

## 1. Prerequisites

You'll need on your machine:

| Tool | Why | Install |
|---|---|---|
| Docker + Compose v2 | Runs Postgres, Qdrant. | https://docs.docker.com/get-docker/ |
| Rust 1.85+ (edition 2024) | Builds the workspace. | `rustup default stable` |
| `just` | Task runner for DB migrations. | `cargo install just` |
| `sqlx-cli` (with postgres) | DB migrations. | `cargo install sqlx-cli --no-default-features --features rustls,postgres` |
| `psql` | Optional — inspect `mr_reviews` rows. | Usually with Postgres client tools. |
| `jq` | Optional — pretty-print JSON from `/usage`. | Most package managers. |
| `ngrok` | Optional — public URL for webhook tests (Advanced section). | https://ngrok.com/download |

**Hardware (for the full-Ollama setup):**
- 12 GB free RAM (`qwen2.5-coder:7b` ≈ 5 GB, `bge-m3` ≈ 1.5 GB, plus headroom)
- ~10 GB free disk for models + indexed chunks
- An x86_64 Linux/macOS box or Apple Silicon

**Clone the repo** if you haven't already:

```bash
git clone https://github.com/<your-org>/mr-ai-backend
cd mr-ai-backend
```

## 2. Start the infrastructure

```bash
docker compose up -d postgres qdrant
```

Verify both are healthy:

```bash
docker compose ps
# postgres → healthy
# qdrant   → healthy
```

If you want a pgAdmin web UI on `:5050` (read-only browsing of
`mr_reviews`, `jobs`, `webhook_events`):

```bash
docker compose --profile dev up -d pgadmin
# Default creds: admin@local / admin (override via PGADMIN_*).
```

## 3. Install Ollama and pull models

Ollama runs models locally. The Quick Start uses **full-Ollama** —
review never leaves your machine.

```bash
# macOS / Linux installer
curl -fsSL https://ollama.com/install.sh | sh

# Pull the two models we need
ollama pull bge-m3            # embeddings, ~1.5 GB
ollama pull qwen2.5-coder:7b  # smart-tier review, ~4.7 GB

# Verify
ollama list
```

Keep Ollama running in the background — it listens on `:11434`.

> **Lower-end laptop?** Swap `qwen2.5-coder:7b` for `llama3.2:3b`
> (~2 GB RAM) — faster but noticeably weaker review comments.
> Update `LLM_SMART_MODEL=llama3.2:3b` in the next step.
> **Online OpenAI alternative?** See "Switching to OpenAI" at the
> end of this page.

## 4. Configure

### `.env`

Create a `.env` next to `Cargo.toml`. Minimum viable contents:

```bash
cat > .env << 'EOF'
# === Logging ===
LOG_LEVEL=info

# === LLM tiers — all-Ollama ===
LLM_FAST_PROVIDER=ollama
LLM_FAST_MODEL=qwen2.5-coder:7b
LLM_FAST_MAX_TOKENS=2048
LLM_FAST_TEMPERATURE=0.2

LLM_SMART_PROVIDER=ollama
LLM_SMART_MODEL=qwen2.5-coder:7b
LLM_SMART_MAX_TOKENS=4096
LLM_SMART_TEMPERATURE=0.2

LLM_EMBED_PROVIDER=ollama
LLM_EMBED_MODEL=bge-m3

OLLAMA_URL=http://localhost:11434

# === Qdrant ===
QDRANT_URL=http://localhost:6334
QDRANT_COLLECTION=mr_ai_code
EMBEDDING_DIM=1024
QDRANT_BATCH_SIZE=64

# === Postgres ===
POSTGRES_DB=mr_ai
POSTGRES_USER=mr_ai
POSTGRES_PASSWORD=change-me-quickstart
DATABASE_URL=postgres://mr_ai:change-me-quickstart@localhost:5432/mr_ai
DATABASE_OPTIONAL=false

# === Auth ===
TRIGGER_SECRET=change-me-quickstart-token

# === Worker ===
WORKER_POOL_SIZE=2
WORKER_POLL_INTERVAL_MS=500

# === Git service ===
GIT_CACHE_DIR=code_data/git_cache
WORKTREE_DIR=code_data/worktrees

# === Project config ===
PROJECTS_CONFIG=projects.toml

# === Secrets backend ===
SECRET_PROVIDER=env

# === Provider creds — use your real GitLab/GitHub PAT ===
GIT_TOKEN=glpat-xxxxxxxxxxxxxxxx           # or ghp_xxxx for GitHub
GIT_API_BASE=https://gitlab.com/api/v4    # or https://api.github.com

# === Webhook secrets — only needed if you set up webhooks ===
# GITLAB_WEBHOOK_SECRET=change-me-gitlab-webhook
# GITHUB_WEBHOOK_SECRET=change-me-github-webhook
EOF
```

**Replace before deploying anywhere real:**
- `POSTGRES_PASSWORD` and `DATABASE_URL` password — generate a random string.
- `TRIGGER_SECRET` — long random token; used as `X-Admin-Token` on every operator call.
- `GIT_TOKEN` + `GIT_API_BASE` — point at the host of the repo you want to review.

### `projects.toml`

For the Quick Start, **start with a single repo** (you'll expand to a
monorepo in the Advanced section). Paste your Flutter project's git
URL:

```toml
[[project]]
slug = "quickstart"
name = "Quick Start Demo"

  [[project.repo]]
  provider = "gitlab"          # gitlab | github | bitbucket
  remote_url = "git@gitlab.com:your-org/your-flutter-app.git"
  default_branch = "main"
  is_primary = true
```

> **No Flutter repo handy?** Fork any public Flutter project (e.g.
> `https://github.com/flutter/samples`) to your own account and use
> that URL. Avoid huge monorepos for the Quick Start — anything
> under ~500 source files will index in under five minutes on a
> laptop.

The `provider` must match the host:
- `gitlab.com` / GitLab self-hosted → `"gitlab"`
- `github.com` / GitHub Enterprise → `"github"`
- Bitbucket Server / Cloud → `"bitbucket"`

## 5. Migrate the database and run

```bash
just db-migrate              # applies pending migrations
cargo run --release          # first build ~5 min; subsequent runs ~5 sec
```

On a successful boot you'll see:

```
✅ projects.toml synced (1 project group(s))
✅ Qdrant collection 'mr_ai_code' ready
✅ Prometheus recorder installed
Server is listening on: 0.0.0.0:8080
```

If `Qdrant collection ... ready` is missing or the line says
`ensure_collection failed`, Qdrant isn't reachable — re-check
`docker compose ps qdrant`.

## 6. First indexing

The worker pool is already running inside the same process as the
API. Trigger a fan-out reindex:

```bash
curl -X POST http://localhost:8080/admin/reindex_all \
  -H "X-Admin-Token: change-me-quickstart-token" \
  -H "X-Project-Slug: quickstart" \
  -d '{}'
```

**Expected response:**
```json
{
  "data": {
    "kind": "Reindex",
    "project_slug": "quickstart",
    "enqueued": [{"job_id": "...", "remote_url": "..."}]
  }
}
```

### Verify the index finished

Pick any of these — they show the same thing from three angles:

**Logs:**
```bash
tail -f logs/mr-ai.$(date +%Y-%m-%d) | grep -E "Reindex|upsert finished"
```
Wait for `Reindex: vector upsert finished upserted=N kept=0 deleted=0`.

**Postgres:**
```bash
docker compose exec postgres psql -U mr_ai -d mr_ai \
  -c "SELECT kind, status, count(*) FROM jobs GROUP BY kind, status;"
```
Look for `Reindex | done | 1` (or more).

**Dashboard:**
```bash
curl -s http://localhost:8080/health/dashboard | jq '.jobs.done, .jobs.failed'
```

## 7. First review

Pick any **open** MR / PR on the repo you indexed. You'll need its
numeric IID (GitLab MR IID, GitHub PR number).

```bash
curl -X POST http://localhost:8080/trigger_git_mr \
  -H 'content-type: application/json' \
  -H "X-Admin-Token: change-me-quickstart-token" \
  -H "X-Project-Slug: quickstart" \
  -d '{"mr_iid":42, "project":"your-org/your-flutter-app"}'
```

(The `project` field is the provider-side slug, e.g.
`acme/flutter-app`. The same value you'd use in a GitLab URL.)

### Inspect the result

```bash
docker compose exec postgres psql -U mr_ai -d mr_ai -c \
  "SELECT status, jsonb_pretty(bundle) FROM mr_reviews
   ORDER BY created_at DESC LIMIT 1;" | less
```

You'll see a JSON blob with the review request, the RAG context that
fed the prompt, and per-target rerank scores. Status should be
`published`.

### How much did it cost?

```bash
curl -s http://localhost:8080/usage | jq
```

Output shape:
```json
{
  "data": {
    "total_calls": 12,
    "total_tokens": 8421,
    "total_cost_usd": 0.0
  }
}
```

(`total_cost_usd` stays `0.0` because Ollama is free — see the
Monitoring guide for cost tracking when you switch to OpenAI.)

---

## 8. (Advanced) Expand to a Flutter monorepo

The cross-repo MR review feature (M1–M5) lets one project span N
repos. Add a `packages` repo alongside `app`, declare a dependency
edge, and reviews on either side automatically pull context from the
other.

### Update `projects.toml`

```toml
[[project]]
slug = "quickstart"
name = "Quick Start Demo"

  [[project.repo]]
  provider = "gitlab"
  remote_url = "git@gitlab.com:your-org/your-flutter-app.git"
  default_branch = "main"
  is_primary = true

  [[project.repo]]
  provider = "gitlab"
  remote_url = "git@gitlab.com:your-org/your-flutter-packages.git"
  default_branch = "main"

  # One edge is enough — the overlay walker is bi-directional.
  [[project.dependency]]
  from = "git@gitlab.com:your-org/your-flutter-app.git"
  to   = "git@gitlab.com:your-org/your-flutter-packages.git"
  kind = "manual"
```

Restart the API (`Ctrl+C` then `cargo run --release` again) — the new
repo + edge get replicated to Postgres on boot.

### Re-index both repos

```bash
curl -X POST http://localhost:8080/admin/reindex_all \
  -H "X-Admin-Token: change-me-quickstart-token" \
  -H "X-Project-Slug: quickstart" -d '{}'
```

### Trigger a review on the packages repo

The same `feat/x` branch on **both** `app` and `packages`? The worker
auto-discovers the linked MR via `list_open_mrs_by_branch` and pulls
the sibling's diff into the prompt:

```bash
curl -X POST http://localhost:8080/trigger_git_mr \
  -H 'content-type: application/json' \
  -H "X-Admin-Token: change-me-quickstart-token" \
  -H "X-Project-Slug: quickstart" \
  -d '{"mr_iid":7, "project":"your-org/your-flutter-packages"}'
```

Inspect the `cross_repo_discovery` audit:

```bash
docker compose exec postgres psql -U mr_ai -d mr_ai -c \
  "SELECT bundle->'cross_repo_discovery' FROM mr_reviews
   ORDER BY created_at DESC LIMIT 1;"
```

You'll see `linked_mrs: [...]` if a sibling MR existed on the same
branch. The prompt now contains a `LINKED_MR_DIFFS` block — open the
bundle to see it inline.

> **No sibling MR found?** That's also valid (Case 1 / Case 2 in the
> [multi-repo-review docs](../services/multi-repo-review.md)). The
> overlay still pulls the sibling repo at `default_branch` so the LLM
> sees cross-repo context.

### Real webhooks via ngrok

To wire a real GitLab/GitHub webhook to your local API:

```bash
# Terminal 1 — keep the API running
cargo run --release

# Terminal 2 — expose port 8080
ngrok http 8080
```

`ngrok` prints a `https://abcd-12-34-56-78.ngrok-free.app` URL. In
GitLab/GitHub:

1. Set `GITLAB_WEBHOOK_SECRET=change-me-gitlab-webhook` in `.env` and
   restart. (Or `GITHUB_WEBHOOK_SECRET` if on GitHub.)
2. In the provider UI: project → Settings → Webhooks → Add webhook.
3. URL: `https://<your-ngrok>.ngrok-free.app/webhooks/gitlab` (or
   `/webhooks/github`).
4. Secret token: the **exact** value you put in `.env`.
5. Trigger events: ✅ Push, ✅ Merge request (GitLab) / ✅ Pushes, ✅
   Pull requests (GitHub).

See [Webhooks](webhooks.md) for the full UI walkthrough including
Bitbucket.

### Publish review comments back into the MR

Once webhooks fire, opt into inline comment publishing:

```bash
# Add to .env, then restart
REVIEW_PUBLISH_COMMENTS=true
```

Open a new MR / push a commit to an existing branch — within ~30
seconds the bot replies with inline comments inside the diff.
**Disable this in shared dev environments** to avoid spamming MRs
with test runs.

---

## Switching to OpenAI

If Ollama is too slow or quality isn't enough, swap the smart and/or
embed tiers:

```bash
LLM_SMART_PROVIDER=openai
LLM_SMART_MODEL=gpt-4o-mini       # ~$0.0005 per typical MR review
OPENAI_API_KEY=sk-...

LLM_EMBED_PROVIDER=openai
LLM_EMBED_MODEL=text-embedding-3-small
EMBEDDING_DIM=1536                # MUST match the model

# Drop the Ollama URL — only Bedrock/OpenAI fallback envs needed
```

`EMBEDDING_DIM` mismatches between the model and Qdrant collection
cause runtime errors at the first upsert — wipe the collection
(`docker compose down -v qdrant`) when you switch.

See [Add a new LLM Provider](add-llm-provider.md) for Anthropic,
Bedrock, and custom backends.

---

## Next steps

- **[Monitoring](monitoring.md)** — health endpoints, Prometheus
  scraping, token-cost tracking, alert rules.
- **[Multi-repo review](../services/multi-repo-review.md)** — the
  full cross-repo design when your project federates >2 repos or
  spans multiple providers.
- **[Operations](../operations.md)** — production deployment
  checklist (env vars, RLS, persistence sizing, audit retention).
- **[Debugging](debugging.md)** — where to look when a webhook
  doesn't fire, a review fails, or a sibling repo is silently
  skipped.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `503 WEBHOOK_SECRET_UNSET` | Per-provider secret not set | Set `GITLAB_WEBHOOK_SECRET` / `GITHUB_WEBHOOK_SECRET` and restart. |
| `400 MISSING_PROJECT_SLUG` | Admin call missing `X-Project-Slug` | Add the header with the slug from `projects.toml`. |
| Reindex stays `queued` forever | Worker pool not started | Check `WORKER_POOL_SIZE > 0`, verify `Server is listening` line in logs. |
| `ensure_collection failed` | Qdrant unreachable | `docker compose ps qdrant`; check `QDRANT_URL=http://localhost:6334`. |
| Review hangs > 5 min on Ollama | Smart-tier model too big | Switch `LLM_SMART_MODEL` to `llama3.2:3b` or move smart tier to OpenAI. |
| `embedding_dim mismatch` | Model dim ≠ `EMBEDDING_DIM` | Reset Qdrant (`docker compose down -v qdrant`) after changing models. |

Full troubleshooting matrix: [Debugging](debugging.md).
