# 🤖 MR-AI Backend

A **self-hosted backend** for automated Merge Request (MR) reviews using local/custom AI models.
Works with **GitHub**, **GitLab**, and other Git providers over **SSH**.
Default models: embeddings — `dengcao/Qwen3-Embedding-0.6B:Q8_0`, generation — `qwen3:32b` (you can change them in settings).

---

## 🧭 Table of Contents

* [Capabilities](#-capabilities)
* [Architecture & Directories](#-architecture--directories)
* [Requirements](#-requirements)
* [Quick Start (Docker)](#-quick-start-docker)
* [Local Install (Rust)](#-local-install-rust)
* [Environment Setup (.env)](#-environment-setup-env)
* [SSH Access to Git](#-ssh-access-to-git)
* [Step-by-Step Workflow](#-step-by-step-workflow)
* [API (cURL Examples)](#-api-curl-examples)
* [AST/Graph Generation](#-astgraph-generation)
* [Practices & Recommendations](#-practices--recommendations)
* [.gitignore](#-gitignore)
* [Switch / Validate Models](#-switch--validate-models)
* [Qdrant GPU Images](#-qdrant-gpu-images)
* [Contributing & License](#-contributing--license)

---

## ✨ Capabilities

* 🔍 Clones code over SSH and prepares a RAG context
* 🌳 Builds syntax trees and code graphs (Tree-sitter)
* 🧠 Indexes code into a vector DB (Qdrant)
* 💬 Answers code questions (LLM via Ollama)
* 🔗 Supports GitHub/GitLab and others via SSH

---

## 🗂️ Architecture & Directories

```bash
├── api/                 # HTTP API server
├── services/            # Services & utilities
├── contextor/           # Answer orchestration: query RAG → LLM response
├── code_data/           # Project data: clones, artifacts, indexes
├── codegraph-prep/      # Build syntax trees and code graph (primary module)
├── graph-prepare/       # Historical module; use codegraph-prep instead
├── vector-lib/          # Vector data helpers
├── rag-store/           # Convert code graph into vector DB format
├── ssh_keys/            # SSH keys for repo access
├── .env                 # Project environment variables
├── docker-compose.yml   # Ollama + Qdrant services
├── bootstrap_ollama.sh  # Helper to spin up dependencies via docker-compose
```

> ℹ️ **Inconsistencies fixed:** unified ports/URLs, single graph module (`codegraph-prep`), consistent env names.

---

## 🧩 Requirements

* 🐳 Docker / Docker Compose — easiest way to start
* 🦀 Rust (stable) — if running the API without Docker
* 📦 \~8–30 GB free disk space (models + indexes)
* 🔐 SSH access to your Git repositories

---

## 🚀 Quick Start (Docker)

1. **Create and adjust `.env`** (see template below).
2. **Start dependencies** (Ollama + Qdrant):

   ```bash
   chmod +x ./bootstrap_ollama.sh
   ./bootstrap_ollama.sh
   # custom names/files:
   ./bootstrap_ollama.sh -f docker-compose.yml -n ollama
   ```
3. **Check Qdrant:**

   * UI/HTTP: [http://localhost:6333](http://localhost:6333)
   * Health check:

     ```bash
     curl -s http://localhost:6333/readyz
     ```
4. **Run the API (if not containerized):**

   ```bash
   cargo run --release
   ```

---

## 🛠 Local Install (Rust)

1. Install Rust and system deps (cmake, build tools, etc.).
2. Configure `.env`.
3. Run:

   ```bash
   cargo run --release
   ```

---

## ⚙️ Environment Setup (.env)

```env
############################
# 🔹 General
############################
PROJECT_NAME=project_x
API_ADDRESS=0.0.0.0:3000

############################
# 🔹 Ollama / LLM
############################
# Default Ollama port: 11434
OLLAMA_HOST=http://localhost
OLLAMA_PORT=11434
OLLAMA_URL=${OLLAMA_HOST}:${OLLAMA_PORT}
OLLAMA_MODEL=qwen3:32b

############################
# 🔹 Embeddings
############################
EMBEDDING_MODEL=dengcao/Qwen3-Embedding-0.6B:Q8_0
EMBEDDING_DIM=1024            # Verify this matches the model (see below)
EMBEDDING_CONCURRENCY=4

############################
# 🔹 Qdrant (Vector DB)
############################
QDRANT_HTTP_PORT=6333
QDRANT_GRPC_PORT=6334
QDRANT_URL=http://localhost:${QDRANT_HTTP_PORT}
QDRANT_COLLECTION=mr_ai_code
QDRANT_DISTANCE=Cosine
QDRANT_BATCH_SIZE=256

############################
# 🔹 Chunking
############################
CHUNK_MAX_CHARS=4000
CHUNK_MIN_CHARS=16

############################
# 🔹 Graph Export
############################
GRAPH_EXPORT_DIR_NAME=graphs_data
GRAPH_EXCLUDE_GENERATED=true
GRAPH_GENERATED_GLOBS=**/*.g.dart,**/*.freezed.dart

############################
# 🔹 Debug
############################
# RUST_BACKTRACE=1
# AST_TARGET_SUFFIX=packages/home_feature/lib/src/presentation/ui/base_home_page.dart
```

> 💡 You can run **multiple projects** by changing `PROJECT_NAME`; each gets its own directory under `code_data/`.

---

## 🔐 SSH Access to Git

### 1) Generate a key

```bash
ssh-keygen -t ed25519 -C "bot@mr-ai.com" -f ./ssh_keys/bot_key
```

Creates:

* private: `ssh_keys/bot_key`
* public:  `ssh_keys/bot_key.pub`

> ⚠️ Never commit private keys.

### 2) Add the public key to your provider

* **GitHub:** Settings → *SSH and GPG Keys* → *New SSH Key*
* **GitLab:** User Settings → *SSH Keys*
  Paste the contents of `ssh_keys/bot_key.pub`.

### 3) Accept host fingerprints (for `libgit2`)

```bash
ssh-keyscan gitlab.com >> ~/.ssh/known_hosts
# add others as needed (github.com, bitbucket.org, etc.)
```

---

## 🧪 Step-by-Step Workflow

1. **Start dependencies** (Ollama + Qdrant) → `./bootstrap_ollama.sh`
2. **Run the API** → `cargo run --release`
3. **Attach repository** → `POST /upload_project_data` with SSH URL(s)
4. **Learn code** → `POST /learn_code`
5. **Prepare code graph** → `POST /prepare_graph`
6. **Initialize Qdrant** → `POST /prepare_qdrant`
7. **Ask questions about the code** → `POST /ask_question`

---

## 🛰️ API (cURL Examples)

> Base URL comes from `API_ADDRESS` (default `0.0.0.0:3000`).

**Ask a question about the code**

```bash
curl --location 'http://0.0.0.0:3000/ask_question' \
--header 'Content-Type: application/json' \
--data '{
  "question": "How can I replace the icon in the navigation bar for the Games section in AppHomePage?"
}'
```

**Attach repository(ies)**

```bash
curl --location 'http://0.0.0.0:3000/upload_project_data' \
--header 'Content-Type: application/json' \
--data-raw '{
  "urls": ["git@gitlab.com:kulllgar/testprojectmain.git"]
}'
```

**Learn code**

```bash
curl --location 'http://0.0.0.0:3000/learn_code'
```

**Prepare graph**

```bash
curl --location 'http://0.0.0.0:3000/prepare_graph'
```

**Initialize Qdrant**

```bash
curl --location 'http://0.0.0.0:3000/prepare_qdrant'
```

---

## 🌳 AST/Graph Generation

Uses [Tree-sitter](https://tree-sitter.github.io/tree-sitter) to parse code and build the graph.

**Supported languages:**

* ✅ Dart (ready)
* 🚧 Rust, Python, JavaScript, TypeScript (in progress)

**Artifacts location:**

```
code_data/<PROJECT_NAME>/graphs_data/<timestamp>/
```

Contents:

* `graph.graphml` — open in [Gephi](https://gephi.org/)
* `ast_nodes.jsonl`, `graph_nodes.jsonl`, `graph_edges.jsonl`
* `summary.json` — metadata

### Dart AST Debugging

**Env vars**

* `PROJECT_NAME` — required; code root is `code_data/{PROJECT_NAME}`
* `AST_TARGET_SUFFIX` — path suffix to the Dart file, e.g.
  `lib/features/splash/presentation/state/splash_state.dart`

**Run**

```bash
PROJECT_NAME=project_x \
AST_TARGET_SUFFIX=lib/features/splash/presentation/state/splash_state.dart \
cargo run --bin dart-ast-dump
```

---

## ✅ Practices & Recommendations

* 🔒 **Secrets:** keep keys in `ssh_keys/`; **never commit** private ones.
* 🧹 **Clean diffs:** ignore artifact folders (`code_data/`, `ssh_keys/`).
* 📏 **Chunking:** tune `CHUNK_MAX_CHARS` / `CHUNK_MIN_CHARS` per language/size.
* 🧪 **Embedding size:** ensure `EMBEDDING_DIM` matches the embedding model (see below).
* 🧩 **Module duplication:** prefer `codegraph-prep` (over `graph-prepare`).

---

## 📝 .gitignore

```gitignore
# Project data & artifacts
code_data/*
!code_data/.gitkeep

# SSH keys
ssh_keys/*
!ssh_keys/.gitkeep

# Local env & build
.env
target/
```

---

## 🔄 Switch / Validate Models

**Install or switch models in Ollama:**

```bash
docker exec -it ollama ollama pull dengcao/Qwen3-Embedding-0.6B:Q8_0
docker exec -it ollama ollama pull qwen3:32b
```

**Validate embedding dimensionality (`EMBEDDING_DIM`):**

```bash
curl --location 'http://localhost:11434/api/embed' \
  --header 'Content-Type: application/json' \
  --data '{
    "model": "dengcao/Qwen3-Embedding-0.6B:Q8_0",
    "input": "hello"
  }'
# response contains "embedding": [...] — check the vector length
```

---

## ⚡ Qdrant GPU Images

Available images:

* **NVIDIA** — `qdrant/qdrant:gpu-nvidia-latest`
* **AMD ROCm** — `qdrant/qdrant:gpu-amd-latest`

> See Qdrant docs for full GPU configuration details.

---

## 🤝 Contributing & License

PRs welcome: new language support, performance improvements, bug fixes.
Open an issue/PR describing your changes.

**License:** FSL-1.1.

---
