# 🤖 MR-AI Backend

A **self-hosted backend** for automated Merge Request (MR) reviews powered by **local AI models**, **Qdrant**, and **Rust**.  
It integrates with **GitLab / GitHub** via **SSH**, uses **RAG over your codebase**, and posts review comments back to MRs.

> 📚 **Full developer documentation** lives in [`docs/`](docs/README.md) —
> architecture, per-service deep-dives, configuration reference, how-tos
> for adding new LLM providers, and observability guides. Start with
> [docs/guides/getting-started.md](docs/guides/getting-started.md).

---

## 🧭 Table of Contents

- [Overview](#-overview)
- [Architecture](#-architecture)
- [Requirements](#-requirements)
- [Quick Start (Docker Stack)](#-quick-start-docker-stack)
- [Environment Configuration](#-environment-configuration)
- [SSH Setup](#-ssh-setup)
- [Review Workflow](#-review-workflow)
- [HTTP API Examples](#-http-api-examples)
- [AI / LLM Service](#-ai--llm-service)
- [Models & Qdrant](#-models--qdrant)
  - [Per-model Ollama rollout](#-per-model-ollama-rollout)
- [Best Practices](#-best-practices)
- [Contributing & License](#-contributing--license)

---

## 🌐 Overview

MR-AI Backend does the following:

- 🐙 **Clones repositories** over SSH (no vendor-specific lock-in).
- 🌲 Builds **AST / LSP-style views** of your project for smarter context.
- 🧠 Stores code chunks and metadata in **Qdrant** (vector DB).
- 🤝 Integrates with **GitLab MRs** via a trigger endpoint.
- 🧾 Generates **precise AI review comments** from local or remote LLMs.

Everything runs in your own environment: **no code leaves your infrastructure**.

---

## 🗂 Architecture

> Main crates and directories in the workspace:

```bash
├── ai-llm-service/      # Shared LLM client layer (Ollama / OpenAI / others)
├── ai-review-engine/    # High-level MR review engine (prompting + MR comments)
├── api/                 # HTTP API server (upload, index, vector search, MR trigger)
├── code-indexer/        # AST / LSP-style indexer: from local code → JSONL artifacts
├── git-context-engine/  # Git/MR context builder + pre-review + prompt assembly
├── project_code_store/  # Git client: clones/updates repos via SSH, no provider API
├── rag-base/            # Vector DB integration: Qdrant collections & search
├── rules/               # Prompt rules: global & per-language review policies
├── temp/                # Temporary debug/diagnostic reports (safe to clean up)
├── code_data/           # Local project data (clones, AST/JSONL, vector artifacts)
├── ssh_keys/            # SSH keys for repo access (never commit private keys)
├── .env                 # Runtime configuration (not in VCS)
├── docker-compose.yml   # Local Docker stack (Ollama + Qdrant)
````

> 🔎 Graph tooling (Gephi, `.graphml`, etc.) is **not required**.
> The indexer focuses on AST/LSP artifacts used by the RAG + review engine.

---

## 💻 Requirements

* 🐳 **Docker + Docker Compose** (for the local stack: Ollama + Qdrant).
* 🦀 **Rust (stable)** for building and running the backend.
* 💽 **8–30 GB free disk space** (models, embeddings, indices, cloned repos).
* 🔐 **SSH access** to your Git provider (GitLab / GitHub / self-hosted).

> ⚠️ **GPU note:** GPU support for Ollama/Qdrant **inside Docker** can be fragile.
> For serious GPU workloads, **prefer native host installation** of Ollama and GPU-enabled Qdrant.

---

## 🚀 Quick Start (Docker Stack)

1. **Start Ollama + Qdrant**:

   ```bash
   docker-compose up -d
   ```

2. **Pull the embedding model inside the Ollama container**
   (uses the model from your environment):

   ```bash
   # Example: EMBEDDING_MODEL might be "bge-m3"
   docker exec -it ollama ollama pull "$EMBEDDING_MODEL"
   ```

3. **Build & run the API service**:

   ```bash
   rustup default stable
   cargo run --release
   ```

4. The HTTP API will listen on `API_ADDRESS` from `.env`
   (by default: `0.0.0.0:3000`).

---

## ⚙️ Environment Configuration

Create an `.env` in the repository root.
Values below are **examples** – adjust for your setup.

```env
############################
# 🔹 Project Settings
############################
PROJECT_NAME=project_x
API_ADDRESS=0.0.0.0:3000

############################
# 🔹 Git Provider (GitLab example)
############################
# Base Git API endpoint
GIT_API_BASE=https://gitlab.com/api/v4

# Access token (project or personal)
# ⚠️ Do NOT commit real tokens. Use CI/CD secrets or a secret manager.
GIT_TOKEN=glpat-***************MASKED**************

# Secret shared with your Git provider webhook
TRIGGER_SECRET=super-secret

############################################################
# 🔸 AI — CORE (shared knobs across AI features)
############################################################

# Optional global max tokens (falls back to provider default if unset)
LLM_MAX_TOKENS=2048

############################################################
# 🔸 AI — RAG / Embeddings / Vector DB
############################################################
RAG_DISABLE=false
RAG_TOP_K=30
RAG_TAKE_PER_TARGET=3
RAG_MIN_SCORE=0.35
RAG_MEMO_CAP=64

# Embedding model (Ollama name or remote model ID)
# Example only – choose any compatible embedding model.
EMBEDDING_MODEL=<your-embedding-model>    # e.g. "bge-m3"
EMBEDDING_DIM=1024
EMBEDDING_CONCURRENCY=4

# Qdrant Vector DB
QDRANT_HTTP_PORT=6333
QDRANT_GRPC_PORT=6334
QDRANT_URL=http://localhost:6334
QDRANT_COLLECTION=mr_ai_code
QDRANT_DISTANCE=Cosine
QDRANT_BATCH_SIZE=256

# Clamping for previews / embeddings
CLAMP_EMBED_MAX_CHARS=1600
CLAMP_EMBED_MAX_LINES=80
CLAMP_PREVIEW_MAX_CHARS=800
CLAMP_PREVIEW_MAX_LINES=50

############################################################
# 🔸 AI — Chunking Strategy
############################################################
CHUNK_MAX_CHARS=4000
CHUNK_MIN_CHARS=16

############################################################
# 🔹 AI — SERVICE (LLM client configuration)
############################################################

## --- Ollama ---
# Base HTTP endpoint
OLLAMA_URL=http://localhost:11434
# Optional legacy port variable
OLLAMA_PORT=11434

# Default models (examples — replace with your own choices):
# - SLOW model for refine/verify (quality first)
OLLAMA_MODEL=<your-main-coder-model>          # e.g. "qwen2.5-coder:32b"
# - FAST model for drafting (latency-optimized)
OLLAMA_MODEL_FAST_MODEL=<your-fast-coder>     # e.g. "qwen3:14b"

# Optional per-client timeout in seconds
# OLLAMA_TIMEOUT_SECS=60

## --- OpenAI (disabled by default) ---
# OPENAI_ENDPOINT=https://api.openai.com
# OPENAI_API_KEY=sk-***MASKED***
# OPENAI_MODEL=gpt-4o-mini
# OPENAI_TIMEOUT_SECS=60
```

---

## 🔐 SSH Setup

1. Generate a dedicated key pair:

   ```bash
   ssh-keygen -t ed25519 -C "bot@mr-ai.local" -f ./ssh_keys/bot_key
   ```

2. Add the public key to your Git provider:

   ```bash
   cat ./ssh_keys/bot_key.pub
   # paste into GitLab / GitHub SSH keys UI
   ```

3. Trust your Git host from the machine running MR-AI:

   ```bash
   ssh-keyscan gitlab.com >> ~/.ssh/known_hosts
   # repeat for github.com or your self-hosted domain if needed
   ```

> ⚠️ Never commit `ssh_keys/bot_key`.
> Only the `*.pub` file is meant to be shared.

---

## 🔄 Review Workflow

High-level sequence of operations:

1. 📥 **Clone project from Git via SSH**

   The backend clones the repository into `code_data/<PROJECT_NAME>`.

2. 🧩 **Index project into AST/LSP artifacts**

   The indexer walks the local code, builds AST/LSP-like structures and emits JSONL files.

3. 📚 **Push code into Qdrant**

   The RAG layer converts AST/code chunks into embeddings and writes them into Qdrant.

4. 🔍 **(Optional) Test the vector search**

   You can query Qdrant via the RAG API to verify relevance and scoring.

5. 🧾 **Trigger MR review**

   GitLab (or another system) calls the MR trigger endpoint.
   The backend pulls MR context, prepares prompts using rules + RAG, and posts review comments.

---

## 🌐 HTTP API Examples

All endpoints are served by the `api` crate.

### 1️⃣ Upload / Refresh Project Data

```bash
curl --location 'http://0.0.0.0:3000/upload_project_data' \
  --header 'Content-Type: application/json' \
  --data-raw '{
    "urls": ["git@gitlab.com:kulllgar/testprojectmain.git"]
  }'
```

* Clones or updates the project into `code_data/<PROJECT_NAME>`.

---

### 2️⃣ Index Project into AST / LSP Artifacts

```bash
curl --location 'http://0.0.0.0:3000/project_indexer' \
  --header 'Content-Type: application/json' \
  --data ''
```

* Runs the **code-indexer** over the local clone.
* Produces JSONL artifacts consumed by the RAG layer.

---

### 3️⃣ Populate Vector Database

```bash
curl --location 'http://0.0.0.0:3000/vector_base_index' \
  --header 'Content-Type: application/json' \
  --data ''
```

* Reads indexer artifacts and writes embeddings into **Qdrant** collection `QDRANT_COLLECTION`.

---

### 3️⃣* 🔍 Test Vector Search (Optional)

```bash
curl --location 'http://0.0.0.0:3000/search_vector_base' \
  --header 'Content-Type: application/json' \
  --data '{
    "query": "How does the app decide when to redirect the user from the splash screen to the /games route?",
    "k": 40
  }'
```

* Debug endpoint to inspect which code fragments your query hits.

---

### 4️⃣ 🔔 Trigger MR Review

```bash
curl --location 'http://0.0.0.0:3000/trigger_git_mr' \
  --header 'Content-Type: application/json' \
  --data '{
    "project_id": "72556530",
    "mr_iid": 2,
    "secret": "super-secret"
  }'
```

* Validates the shared `secret`.
* Pulls MR details via `GIT_API_BASE` and `GIT_TOKEN`.
* Uses `git-context-engine` + `ai-review-engine` to:

  * build focused prompts per diff hunk,
  * query LLM(s),
  * and post comments back to the MR.

---

## 🧠 AI / LLM Service

The `ai-llm-service` crate provides a **shared LLM abstraction layer**:

* Works with **Ollama** and optionally **OpenAI** (or other HTTP-compatible providers).
* Manages **three logical profiles**:

  * `fast` – low latency, for drafting.
  * `slow` – higher quality, for refinement/verification.
  * `embedding` – for vectorization.
* Reuses HTTP clients and configuration across the whole backend.
* Exposes **health checks** so the API can validate LLM readiness on startup.
* Uses **structured logging** (`tracing`) with model/provider metadata for debugging.

Each profile uses the model defined in `.env`:

* `OLLAMA_MODEL_FAST_MODEL` → fast profile.
* `OLLAMA_MODEL` → slow profile (falls back to `fast` if unset).
* `EMBEDDING_MODEL` → embedding profile.

---

## 🧱 Models & Qdrant

### 🧩 Per-model Ollama rollout

You need to **pull all models** used by MR-AI into Ollama.

#### ✅ Recommended: native host installation (better GPU support)

If Ollama is installed directly on the host:

```bash
# Embedding model
ollama pull "$EMBEDDING_MODEL"          # e.g. bge-m3

# Main high-quality coding model (SLOW profile)
ollama pull "$OLLAMA_MODEL"             # e.g. qwen2.5-coder:32b

# Fast coding model (FAST profile)
ollama pull "$OLLAMA_MODEL_FAST_MODEL"  # e.g. qwen3:14b
```

Concrete example (adjust to your own choices):

```bash
ollama pull bge-m3
ollama pull qwen2.5-coder:32b
ollama pull qwen3:14b
```

#### 🐳 Docker-based Ollama (GPU can be tricky)

If you're using the provided Docker stack:

```bash
# Embedding model
docker exec -it ollama ollama pull "$EMBEDDING_MODEL"

# Main high-quality coding model (SLOW profile)
docker exec -it ollama ollama pull "$OLLAMA_MODEL"

# Fast coding model (FAST profile)
docker exec -it ollama ollama pull "$OLLAMA_MODEL_FAST_MODEL"
```

> ⚠️ GPU acceleration inside Docker may require additional runtime configuration
> (drivers, runtime flags, container runtime). For stable GPU workloads,
> prefer **native Ollama** installation on the host when possible.

You can also validate embedding dimensionality:

```bash
curl -s http://localhost:11434/api/embed \
  -H 'Content-Type: application/json' \
  -d '{"model":"bge-m3","input":"hello"}'
```

Make sure `EMBEDDING_DIM` matches the vector size returned by your embedding model.

---

### 💽 Qdrant (CPU & GPU)

* Default Docker image (CPU): `qdrant/qdrant:latest`
* GPU images:

  * NVIDIA — `qdrant/qdrant:gpu-nvidia-latest`
  * AMD ROCm — `qdrant/qdrant:gpu-amd-latest`

> ⚠️ As with Ollama, GPU inside Docker may require extra setup
> (runtime flags, drivers, container runtime). For production GPU loads,
> consider running Qdrant with direct host GPU access.

---

## ✅ Best Practices

* 🔒 **Secrets**: never commit real tokens/keys; rely on CI/CD or a secret manager.
* 📦 **Pre-pull models** for Ollama so the first MR review isn’t slowed by downloads.
* 📏 **Tune chunking** (`CHUNK_MAX_CHARS`, `CHUNK_MIN_CHARS`) per language & repo size.
* 🧪 **Align embedding dimension** (`EMBEDDING_DIM`) with your actual embedding model.
* 🧠 **RAG tuning**: experiment with `RAG_TOP_K`, `RAG_TAKE_PER_TARGET`, `RAG_MIN_SCORE`.
* 🩺 Use profile health checks during startup to fail fast if LLMs are misconfigured.

---

## 🤝 Contributing & License

Contributions are welcome — language adapters, performance improvements, new rulesets, and bugfixes are highly appreciated.
License: **FSL-1.1**.
