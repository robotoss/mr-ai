# 🤖 MR-AI Backend

A **self-hosted AI-powered backend** for **automated Merge Request (MR) reviews** using custom AI models.
Supports integration with **GitHub**, **GitLab**, and other Git providers via **SSH**.

---

## 📂 Project Structure

```bash
├── api/
├── code_data/
├── graph-prepare/
├── services/
├── ssh_keys/
├── .env
```

---

## ⚙️ Environment Configuration

Configure the service via `.env` file or environment variables:

```env
PROJECT_NAME=test_project      # Unique folder name per project
API_ADDRESS=0.0.0.0:3000       # API server binding address
```

> You can run multiple project services by using different `PROJECT_NAME` values.

---

## 🔐 SSH Setup for Git Access

This service uses **SSH keys** for secure access to private repositories.

### 1️⃣ Generate SSH Key

```bash
ssh-keygen -t ed25519 -C "bot@mr-ai.com" -f ./ssh_keys/bot_key
```

This generates:

* **Private key:** `ssh_keys/bot_key`
* **Public key:** `ssh_keys/bot_key.pub`

> ⚠️ **Never** commit your private key to version control.

---

### 2️⃣ Add Public Key to Git Provider

#### GitHub

1. Go to **Settings → SSH and GPG Keys → New SSH Key**
2. Paste contents of `ssh_keys/bot_key.pub`

#### GitLab

1. Go to **User Settings → SSH Keys**
2. Paste contents of `ssh_keys/bot_key.pub`

---

### 3️⃣ Accept SSH Host Fingerprint (Required for `libgit2`)

```bash
ssh-keyscan gitlab.com >> ~/.ssh/known_hosts
```

This prevents host verification errors during cloning.

---

## 🚀 Running the Service

1. Configure `.env`
2. Set up SSH access
3. Start service:

```bash
cargo run --release
```

---

## 📁 Git Ignore Best Practices

Add to `.gitignore`:

```gitignore
code_data/*
!code_data/.gitkeep
ssh_keys/
!ssh_keys/.gitkeep
```

* `code_data/` — contains dynamically cloned repos (keep empty `.gitkeep`)
* `ssh_keys/` — should **never** be committed

---

## 🌳 Syntax Tree & Graph Generation

We use [Tree-sitter](https://tree-sitter.github.io/tree-sitter) to parse source code into syntax trees, which are then used to build graphs.

### Currently Supported / In Progress

* ✅ Dart *(ready)*
* 🚧 Rust *(in progress)*
* 🚧 Python *(in progress)*
* 🚧 JavaScript *(in progress)*
* 🚧 TypeScript *(in progress)*

> Contributions to support more languages are **very welcome**.

---

## 📦 Saved Artifacts

When processing a project, the following are stored at:

```
code_data/<project_name>/graphs_data/<timestamp>/
```

* `graph.graphml` → import directly into [Gephi](https://gephi.org/)
* `ast_nodes.jsonl` → Abstract syntax tree nodes
* `graph_nodes.jsonl` → Graph node data
* `graph_edges.jsonl` → Graph edge data
* `summary.json` → Summary metadata

---

## 🛠 API Endpoints

1. **Upload Project Data**
   `POST /upload_project_data`
   Send repository data to the service.

2. **Learn Code & Generate Graphs**
   `POST /learn_code`
   Create graph representations of your code.
