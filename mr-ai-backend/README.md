Got it ✅
Here’s a fully polished, best-practices **README.md** with consistent English, emojis, and improved formatting for clarity and developer friendliness.

---

# 🤖 MR-AI Backend

A **self-hosted AI-powered backend** for **automated Merge Request (MR) reviews** using custom AI models.
Supports integration with **GitHub**, **GitLab**, and other Git providers via **SSH**.

---

## 📂 Project Structure

```bash
├── api/                # API server logic
├── code_data/          # Cloned repository data and processing artifacts
├── vector-lib/         # Use to work with vector data
├── graph-prepare/      # Syntax tree and graph generation
├── services/           # Service logic and helpers
├── ssh_keys/           # SSH keys for Git access
├── .env                # Environment configuration
```

---

## ⚙️ Environment Configuration

Configuration is done via a `.env` file or environment variables:

```env
######## General ########
PROJECT_NAME=test_project      # Unique folder name per project
API_ADDRESS=0.0.0.0:3000       # API server binding address

######## Qdrant ########
QDRANT_HTTP_PORT=6333          # Qdrant HTTP API port
QDRANT_GRPC_PORT=6334          # Qdrant gRPC API port
QDRANT_URL=http://localhost:6333
QDRANT_COLLECTION=mr_ai_code
QDRANT_DISTANCE=Cosine
QDRANT_BATCH_SIZE=256

######## Graph / Export ########
GRAPH_EXPORT_DIR_NAME=graphs_data
GRAPH_EXCLUDE_GENERATED=true
GRAPH_GENERATED_GLOBS=**/*.g.dart,**/*.freezed.dart

######## Embeddings ########
OLLAMA_URL=http://localhost:7869
EMBEDDING_MODEL=dengcao/Qwen3-Embedding-0.6B:Q8_0
EMBEDDING_DIM=1024 # Verify via curl API (see below)

######## Chunking ########
CHUNK_MAX_CHARS=4000
CHUNK_MIN_CHARS=16

######## Concurrency ########
EMBEDDING_CONCURRENCY=4
```

> 💡 You can run **multiple projects** by changing `PROJECT_NAME`.

---

## 🔐 SSH Setup for Git Access

The service uses **SSH keys** for secure access to private repositories.

### 1️⃣ Generate SSH Key

```bash
ssh-keygen -t ed25519 -C "bot@mr-ai.com" -f ./ssh_keys/bot_key
```

This will create:

* **Private key:** `ssh_keys/bot_key`
* **Public key:** `ssh_keys/bot_key.pub`

⚠️ **Never commit** your private key to version control.

---

### 2️⃣ Add Public Key to Your Git Provider

**GitHub**

1. Go to **Settings → SSH and GPG Keys → New SSH Key**
2. Paste the contents of `ssh_keys/bot_key.pub`

**GitLab**

1. Go to **User Settings → SSH Keys**
2. Paste the contents of `ssh_keys/bot_key.pub`

---

### 3️⃣ Accept SSH Host Fingerprint (Required for `libgit2`)

```bash
ssh-keyscan gitlab.com >> ~/.ssh/known_hosts
```

---

## 🚀 Running the Service

1. Set up `.env`
2. Configure SSH access
3. Start the service:

```bash
cargo run --release
```

---

## 📁 .gitignore Best Practices

```gitignore
# Dynamic repo data
code_data/*
!code_data/.gitkeep

# Private SSH keys
ssh_keys/*
!ssh_keys/.gitkeep
```

---

## 🌳 Syntax Tree & Graph Generation

Uses [Tree-sitter](https://tree-sitter.github.io/tree-sitter) to parse code into syntax trees and build graphs.

**Languages:**

* ✅ Dart *(ready)*
* 🚧 Rust *(in progress)*
* 🚧 Python *(in progress)*
* 🚧 JavaScript *(in progress)*
* 🚧 TypeScript *(in progress)*

---

## 📦 Saved Artifacts

Stored at:

```
code_data/<project_name>/graphs_data/<timestamp>/
```

Includes:

* `graph.graphml` → Import into [Gephi](https://gephi.org/)
* `ast_nodes.jsonl` → Abstract syntax tree nodes
* `graph_nodes.jsonl` → Graph node data
* `graph_edges.jsonl` → Graph edge data
* `summary.json` → Summary metadata

---

## 🛠 API Endpoints

1. **Upload Project Data**
   `POST /upload_project_data` — Send repository data.
2. **Learn Code & Generate Graphs**
   `POST /learn_code` — Build graph representation of code.

---

## 🐳 Quick Start with Docker Compose

```bash
docker compose up -d
# Open UI / test API:
open http://localhost:6333
# Health check:
curl -s localhost:6333/readyz
```

---

## ⚡ GPU Builds (Linux Only)

GPU builds available for:

* **NVIDIA** — `qdrant/qdrant:gpu-nvidia-latest`
* **AMD ROCm** — `qdrant/qdrant:gpu-amd-latest`

> For full Docker Compose GPU configuration, see the Qdrant docs:
> [Qdrant GPU Guide](https://qdrant.tech/documentation/gpu/)

---

## 🧪 Testing Embedding Model

```bash
docker exec -it ollama ollama pull dengcao/Qwen3-Embedding-0.6B:Q8_0
docker exec -it ollama ollama pull qwen3:32b
```

Check model dimension (`EMBEDDING_DIM`):

```bash
curl --location 'http://localhost:11434/api/embed' \
  --header 'Content-Type: application/json' \
  --data '{
    "model": "dengcao/Qwen3-Embedding-0.6B:Q8_0",
    "input": "hello"
  }'
```

---

## 🤝 Contributing

Contributions for additional language support, performance improvements, and bug fixes are welcome!
Please open an issue or PR.

---

## 📜 License

MIT — Free to use and modify.

---

Do you want me to also **add a badges section** (build status, Docker pulls, version, etc.) so it looks even more professional? That would make this README really pop.
