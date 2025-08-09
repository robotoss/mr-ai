# 🤖 MR-AI Backend

Self-hosted AI-powered backend service for **automated Merge Request (MR) reviews** using custom AI models.
Supports integration with **GitHub**, **GitLab**, and other Git providers over **SSH**.

---

## 📦 Project Structure

```

├── api\_lib         # API interface layer
├── service\_lib     # Core business logic
├── code\_data/      # Cloned Git repositories (auto-managed)
├── ssh\_keys/       # Private SSH keys for repo access
├── .env            # Environment configuration

````

---

## ⚙️ Environment Configuration

Configure the service via `.env` file or environment variables:

```env
PROJECT_NAME=test_project      # Unique folder name per project
API_ADDRESS=0.0.0.0:3000       # API server binding address
````

You can run multiple project services by assigning different `PROJECT_NAME` values.

---

## 🔐 SSH Setup for Git Access

This service uses **SSH keys** to access private Git repositories (GitHub, GitLab, etc.). Follow the steps below to configure secure, headless cloning.

---

### ✅ Step 1: Generate SSH Key

If you don't already have a key:

```bash
ssh-keygen -t ed25519 -C "bot@mr-ai.com" -f ./ssh_keys/bot_key
```

This creates:

* **Private key**: `ssh_keys/bot_key`
* **Public key**: `ssh_keys/bot_key.pub`

> ⚠️ Do **not** commit your private key to version control.

---

### ✅ Step 2: Add Public Key to Git Provider

#### 🔗 GitHub

1. Go to: `GitHub → Settings → SSH and GPG Keys → New SSH Key`
2. Paste the contents of `ssh_keys/bot_key.pub`

#### 🔗 GitLab

1. Go to: `GitLab → User Settings → SSH Keys`
2. Paste the contents of `ssh_keys/bot_key.pub`

---

### ✅ Step 3: Accept SSH Host Fingerprint (Required for libgit2)

Run this once on your host machine:

```bash
ssh-keyscan gitlab.com >> ~/.ssh/known_hosts
```

> This avoids host verification errors during repo cloning.

---

## 🚀 Running the Service

1. Set up your `.env` file.
2. Ensure SSH access is configured.
3. Start the service:

```bash
cargo run --release
```

---

## 📁 Git Ignore Best Practice

In your `.gitignore`, ignore all generated/cloned repo data:

```gitignore
code_data/*
!code_data/.gitkeep
ssh_keys/
!ssh_keys/.gitkeep
```

* `code_data/` is managed dynamically and can contain multiple cloned projects.
* `.gitkeep` ensures the folder is tracked (but empty).
* `ssh_keys/` should **never** be committed.

---



we use tree-sitter (https://tree-sitter.github.io/tree-sitter) for get syntaxis tree for build graphs

at this moment we add work with
rust
python
javascript
typescript
dart
