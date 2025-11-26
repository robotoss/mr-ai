# 🤖 MR-AI Backend

A **self-hosted backend** for automated Merge Request (MR) reviews powered by **local/custom AI models**.
It integrates with **GitHub**, **GitLab**, and other Git providers over **SSH**.
By default it uses [Ollama](https://ollama.com) for local inference and [Qdrant](https://qdrant.tech) as a vector database.

---

## 🧭 Table of Contents

* [Capabilities](#-capabilities)
* [Architecture](#-architecture)
* [Requirements](#-requirements)
* [Quick Start (Docker)](#-quick-start-docker)
* [Local Install (Rust)](#-local-install-rust)
* [Environment Setup](#-environment-setup)
* [SSH Access to Git](#-ssh-access-to-git)
* [Workflow](#-workflow)
* [API (cURL Examples)](#-api-curl-examples)
* [AST/Graph Generation](#-astgraph-generation)
* [AI LLM Service Profiles](#-ai-llm-service-profiles)
* [Practices & Recommendations](#-practices--recommendations)
* [.gitignore](#-gitignore)
* [Model Management](#-model-management)
* [Qdrant GPU Images](#-qdrant-gpu-images)
* [Contributing & License](#-contributing--license)

---

## ✨ Capabilities

* 🔍 Clones repositories over SSH and prepares **RAG** context
* 🌳 Builds **ASTs** and **code graphs** with Tree-sitter
* 🧠 Indexes source code into **Qdrant** (vector DB)
* 💬 Answers questions with **LLM profiles** (fast / slow / embedding)
* 🔗 Supports GitHub/GitLab and others via SSH
* 🩺 Includes a built-in **health service** for LLM endpoints

---

## 🗂️ Architecture

> Key libs and directories currently in the repo:

```bash
├── ai-llm-service/      # Shared LLM services: providers (Ollama/OpenAI), health, profile manager
├── api/                 # HTTP API server (endpoints calling contextor + services)
├── code_data/           # Project data: repo clones, embeddings, graph artifacts
├── codegraph-prep/      # Build ASTs and code graphs (Tree-sitter based)
├── contextor/           # Orchestration: query → RAG → LLM (fast/slow/embedding)
├── mr-reviewer/         # MR review logic (rules execution, scoring, escalation)
├── rag-store/           # Transform AST/graph into vector DB payloads (Qdrant)
├── rules/               # LLM prompt rules: global & per-language policies
├── services/            # Service wiring (e.g., repo fetchers, providers glue)
├── src/                 # Main binaries / entrypoints
├── ssh_keys/            # SSH keys for repo access (do not commit private keys)
├── .env                 # Project environment variables
├── bootstrap_ollama.sh  # Helper to spin up dependencies (Ollama + Qdrant)
├── docker-compose.yml   # Local stack: Ollama + Qdrant
```

---

## 🧩 Requirements

* 🐳 Docker + Docker Compose
* 🦀 Rust (stable) for local runs
* 📦 8–30 GB free disk space (models + embeddings + indices)
* 🔐 SSH access to your Git repositories

---

## 🚀 Quick Start (Docker)

1. **Prepare `.env`** (see below).
2. **Start dependencies** (Ollama + Qdrant):

   ```bash
   chmod +x ./bootstrap_ollama.sh
   ./bootstrap_ollama.sh
   ```
3. **Check Qdrant UI**: [http://localhost:6333](http://localhost:6333)
4. **Run the API** (locally):

   ```bash
   cargo run --release
   ```

---

## 🛠 Local Install (Rust)

```bash
rustup default stable
cargo run --release
```

---

## ⚙️ Environment Setup

> Minimal, clean, and consistent with the current codebase.
> Note: `QDRANT_URL` intentionally remains `http://localhost:6334` per your setup.

```env
############################
# 🔹 Project
############################
PROJECT_NAME=project_x
API_ADDRESS=0.0.0.0:3000

############################
# 🔹 Ollama / LLM
############################
# Local or remote Ollama endpoint
OLLAMA_URL=http://localhost:11434
# Models (fast for drafting, slow for refine/verify)
OLLAMA_MODEL_FAST_MODEL=qwen3:14b
OLLAMA_MODEL=qwen3:32b

############################
# 🔹 Embeddings
############################
EMBEDDING_MODEL=dengcao/Qwen3-Embedding-0.6B:Q8_0
EMBEDDING_DIM=1024
EMBEDDING_CONCURRENCY=4

############################
# 🔹 Qdrant (Vector DB)
############################
QDRANT_HTTP_PORT=6333
QDRANT_GRPC_PORT=6334
QDRANT_URL=http://localhost:6334
QDRANT_COLLECTION=mr_ai_code
QDRANT_DISTANCE=Cosine
QDRANT_BATCH_SIZE=256

############################
# 🔹 Chunking
############################
CHUNK_MAX_CHARS=4000
CHUNK_MIN_CHARS=16

############################
# 🔹 Debug
############################
# RUST_BACKTRACE=1
RUST_LOG=mr_reviewer=debug,contextor=debug,rag_store=debug,reqwest=info
```

Optional (GitLab integration):

```env
# GITLAB_API_BASE=https://gitlab.com/api/v4
# GITLAB_TOKEN=__REDACTED__
# TRIGGER_SECRET=super-secret
```

---

## 🔐 SSH Access to Git

```bash
ssh-keygen -t ed25519 -C "bot@mr-ai.com" -f ./ssh_keys/bot_key
ssh-keyscan gitlab.com >> ~/.ssh/known_hosts
# Repeat for github.com / your host as needed
```

Add `ssh_keys/bot_key.pub` in your Git provider’s SSH keys page.
**Never commit** `ssh_keys/bot_key`.

---

## 🧪 Workflow

1. Start **Ollama + Qdrant** → `./bootstrap_ollama.sh`
2. Run **API** → `cargo run --release`
3. Attach repository → `POST /upload_project_data`
4. Learn code (index) → `POST /learn_code`
5. Prepare graph → `POST /prepare_graph`
6. Initialize Qdrant → `POST /prepare_qdrant`
7. Ask questions → `POST /ask_question`

---

## 🛰️ API (cURL Examples)

**Ask a code question**

```bash
curl --silent 'http://0.0.0.0:3000/ask_question' \
  -H 'Content-Type: application/json' \
  -d '{"question": "Where is the main navigation bar defined?"}'
```

**Attach repository**

```bash
curl --silent 'http://0.0.0.0:3000/upload_project_data' \
  -H 'Content-Type: application/json' \
  -d '{"urls": ["git@gitlab.com:user/project.git"]}'
```

**Learn code**

```bash
curl --silent 'http://0.0.0.0:3000/learn_code'
```

**Prepare graph**

```bash
curl --silent 'http://0.0.0.0:3000/prepare_graph'
```

**Initialize Qdrant**

```bash
curl --silent 'http://0.0.0.0:3000/prepare_qdrant'
```

---

## 🌳 AST/Graph Generation

We rely on **Tree-sitter** to parse code and build graph structures.

**Artifacts** live in:

```
code_data/<PROJECT_NAME>/graphs_data/<timestamp>/
```

Contents:

* `graph.graphml` — open with [Gephi](https://gephi.org/)
* `ast_nodes.jsonl`, `graph_nodes.jsonl`, `graph_edges.jsonl`
* `summary.json` — metadata

---

## 🧠 AI LLM Service

The `ai-llm-service` crate provides a **shared manager** for three LLM profiles:

* **fast** — quick drafting
* **slow** — refinement/verification (falls back to fast if not set)
* **embedding** — vector embeddings

### Features

* **Provider-agnostic** — works with **Ollama** and **OpenAI**
* **Client caching** — reuses HTTP clients per `(provider, endpoint, model, key, timeout)`
* **Async + Arc-safe** — designed for reuse across tasks and modules
* **Built-in health checks** — probes Ollama (`/api/tags`) and OpenAI (`/v1/models`)
* **Unified errors** — normalized error model with snippets
* **Structured logging** — uses [`tracing`](https://docs.rs/tracing), tagged `[AI LLM Service]`

---

### Example

```rust
use std::sync::Arc;
use ai_llm_service::{
    service_profiles::LlmServiceProfiles,
    llm::LlmModelConfig,
    config::llm_provider::LlmProvider,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fast = LlmModelConfig {
        provider: LlmProvider::Ollama,
        model: "qwen3:14b".into(),
        endpoint: "http://localhost:11434".into(),
        api_key: None,
        max_tokens: Some(512),
        temperature: Some(0.7),
        top_p: Some(0.9),
        timeout_secs: Some(30),
    };

    let slow = LlmModelConfig { model: "qwen3:32b".into(), ..fast.clone() };
    let embedding = LlmModelConfig { ..fast.clone() };

    // Create service with health checker (timeout = 10s)
    let svc = Arc::new(LlmServiceProfiles::new(fast, Some(slow), embedding, Some(10))?);

    let txt = svc.generate_fast("Hello world", None).await?;
    println!("FAST: {}", txt);

    let emb = svc.embed("Ferris").await?;
    println!("Embedding size = {}", emb.len());

    let statuses = svc.health_all().await?;
    println!("Health: {:?}", statuses);

    Ok(())
}
```

---

### API

* `generate_fast(prompt, system)` → quick text generation
* `generate_slow(prompt, system)` → refined generation (falls back to fast)
* `embed(input)` → vector embeddings
* `health_all()` → probe all distinct profiles
* `profiles()` → return references to `(fast, slow, embedding)` configs

All methods return `Result<_, AiLlmError>` with normalized provider-specific errors.

---

### Logging

This library uses **structured logs** via `tracing`.
Logs include provider, endpoint, model, and error snippets, e.g.:

```
2025-09-12T12:05:33Z [DEBUG] [AI LLM Service] POST http://localhost:11434/api/generate model="qwen3:14b"
2025-09-12T12:05:34Z [ERROR] [AI LLM Service] Ollama returned 500 at /api/generate snippet="internal error"
```

**What is logged:**

* **INFO** — profile creation, health check results
* **DEBUG** — outgoing HTTP requests (`POST /api/...`)
* **ERROR** — non-2xx responses, decode errors, missing models
* **WARN** — unexpected but non-fatal conditions (e.g. missing `models` field in health check)

You can **tune log level just for this crate** in your `main` with `EnvFilter::add_directive`:

```rust
use tracing_subscriber::{EnvFilter, prelude::*};

fn init_tracing() {
    // Base filter from env or fallback
    let base = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // Raise verbosity for ai-llm-service only
    let filter = base.add_directive("ai_llm_service=debug".parse().unwrap());

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}
```

This way you can keep your app logs at `info` while enabling **detailed debug logs** only for `ai-llm-service`.

---

## ✅ Practices & Recommendations

* 🔒 **Secrets**: never commit real tokens/keys; use secret managers or CI/CD variables
* 📦 **Pre-pull models** into Ollama to reduce first-request latency
* 📏 **Tune chunking** (size vs. accuracy trade-offs)
* 🧪 **Match embedding dimension** (`EMBEDDING_DIM`) to your embedding model
* 🩺 Use `health_all()` during startup readiness checks

---

## 📝 .gitignore

```gitignore
code_data/*
!code_data/.gitkeep
ssh_keys/*
!ssh_keys/.gitkeep
.env
target/
```

---

## 🔄 Model Management

```bash
# Pull models (inside your Ollama container)
docker exec -it ollama ollama pull dengcao/Qwen3-Embedding-0.6B:Q8_0
docker exec -it ollama ollama pull qwen3:14b
docker exec -it ollama ollama pull qwen3:32b

# Validate embedding dimensionality (EMBEDDING_DIM)
curl -s http://localhost:11434/api/embed \
  -H 'Content-Type: application/json' \
  -d '{"model":"bge-m3","input":"hello"}'
```

---

## ⚡ Qdrant GPU Images

* NVIDIA — `qdrant/qdrant:gpu-nvidia-latest`
* AMD ROCm — `qdrant/qdrant:gpu-amd-latest`

---

## 🤝 Contributing & License

Contributions welcome: language support, performance tweaks, bug fixes.
License: **FSL-1.1**.


Run docker for ollama and qdrant:
` docker-compose up -d`

Install embedding model:
`docker exec -it ollama ollama pull bge-m3`


Refactor libs structure:
 - ai-llm-service (New)
 - api (New)
 - code-indexer (New)
 - code-date (New)
 - codegraph-prep (Old)
 - contextor (Old)
 - mr-reviewer (Old)
 - project-code-store (New)
 - rag-base (New)
 - rag-store (Old)
 - rules (New)
 - git-context-engine (New)
 - ai-review-engine (New)
