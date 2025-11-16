# README — Vector RAG Module (Rust + Qdrant)

> **Goal:** Stream CodeChunks from a JSONL file, rebuild a **fresh** Qdrant collection on each run, and expose **top-K semantic search** using a pluggable embedding backend (default: `bge-m3`).

## 1) What this module does (in one paragraph)

On every indexing run the module **drops and recreates** a Qdrant collection, reads `code_chunks.jsonl` line-by-line, builds a compact text representation for each chunk, generates embeddings with the configured model (`bge-m3`, 1024-D), and upserts vectors + metadata into Qdrant in batches. At query time, it embeds the user’s text/code prompt and returns the **K nearest** chunks (default **K = `RAG_TOP_K` = 8**) by cosine similarity with lightweight previews (file, symbol path, snippet).

---

## 2) High-level Architecture (superficial)

```
JSONL (stream) ──► Parser ──► Normalizer ──► Embedder (bge-m3, 1024-D)
                                      │
                                      ▼
                           Qdrant (HNSW, Cosine)
                                      ▲
     Query text ──► Embedder (same) ──┘ ──► top-K results (payload + score)
```

* **Parser (streaming):** reads one JSON object per line from `code_data/out/<PROJECT_NAME>/code_chunks.jsonl`.
* **Normalizer:** builds an embedding text with key signals: language, kind, symbol path, signature, short doc, clamped snippet, imports.
* **Embedder:** `bge-m3` (multilingual) with `EMBEDDING_DIM=1024`, concurrency controlled by `EMBEDDING_CONCURRENCY`.
* **Qdrant:** single vector space `"code"`; **Cosine** distance; upsert in batches.
* **Search API:** top-K (`RAG_TOP_K`) nearest neighbors with preview metadata.

---

## 3) Principle of operation

1. **Full refresh**

   * Check collection; **drop** if exists; **create** with vector size from env (`EMBEDDING_DIM`) and distance from `QDRANT_DISTANCE` (Cosine).
2. **Ingest**

   * Stream JSONL → deserialize CodeChunk → normalize → embed (batched, concurrent) → upsert (`QDRANT_BATCH_SIZE`).
3. **Query**

   * Embed query → `search_points(limit=RAG_TOP_K)` → return scored payloads.

> Deterministic and **no stale data** by design: each run replaces the whole collection.

---

## 4) Data & Payload (what gets stored)

**Vector (name: `code`)**

* Dimension = `EMBEDDING_DIM` (e.g., 1024 for `bge-m3`)
* Distance = `QDRANT_DISTANCE` (`Cosine`)

**Payload (denormalized, for previews/filters)**

* `id` (string; original chunk id) — used as point ID
* `file`, `language`, `kind`, `symbol`, `symbol_path`
* `signature` (optional), `doc` (first line, optional)
* `snippet` (clamped using `CHUNK_MAX_CHARS`)
* `content_sha256`
* `imports` (union of raw imports + graph.imports_out if present)
* optional: `lsp_fqn`, `tags`

---

## 5) Embeddings Model

* **Default:** `bge-m3` (multilingual, robust for code+natural language).
* **Dimension:** `EMBEDDING_DIM=1024`.
* **Concurrency:** `EMBEDDING_CONCURRENCY=4`.
* **Why:** great speed/quality trade-off, good multilingual behavior for mixed code comments, identifiers, and NL queries.

> If you ever switch providers, keep `EMBEDDING_DIM` consistent with the chosen model and recreate the collection.

---

## 6) Configuration (ENV)

These knobs are read at startup—no code changes needed.

### Project / Service

| Key            | Example        | Purpose                                                            |
| -------------- | -------------- | ------------------------------------------------------------------ |
| `PROJECT_NAME` | `project_x`    | Used to resolve default input path / naming                        |
| `API_ADDRESS`  | `0.0.0.0:3000` | Optional HTTP endpoint (if exposing a tiny search API/CLI wrapper) |

### RAG / Search

| Key                   | Default/Example | Notes                                     |
| --------------------- | --------------- | ----------------------------------------- |
| `RAG_DISABLE`         | `false`         | If `true`, indexing/search can be skipped |
| `RAG_TOP_K`           | `8`             | **Top-K** neighbors per query             |
| `RAG_TAKE_PER_TARGET` | `3`             | Optional per-target cap when aggregating  |
| `RAG_MIN_SCORE`       | `0.50`          | Optional similarity threshold for results |
| `RAG_MEMO_CAP`        | `64`            | Optional in-process memoization size      |

### Embeddings

| Key                     | Example  | Notes                              |
| ----------------------- | -------- | ---------------------------------- |
| `EMBEDDING_MODEL`       | `bge-m3` | Embedding model id/name            |
| `EMBEDDING_DIM`         | `1024`   | **Must match** the model dimension |
| `EMBEDDING_CONCURRENCY` | `4`      | Parallel embedding workers         |

### Qdrant

| Key                 | Example                 | Notes                                        |
| ------------------- | ----------------------- | -------------------------------------------- |
| `QDRANT_URL`        | `http://localhost:6334` | **gRPC** endpoint (qdrant-client uses tonic) |
| `QDRANT_HTTP_PORT`  | `6333`                  | FYI (REST), not used by gRPC client          |
| `QDRANT_GRPC_PORT`  | `6334`                  | FYI; ensure `QDRANT_URL` points here         |
| `QDRANT_COLLECTION` | `mr_ai_code`            | Collection to (re)create                     |
| `QDRANT_DISTANCE`   | `Cosine`                | Distance metric                              |
| `QDRANT_BATCH_SIZE` | `256`                   | Upsert batch size                            |

### Chunking

| Key               | Example | Notes                                 |
| ----------------- | ------- | ------------------------------------- |
| `CHUNK_MAX_CHARS` | `4000`  | Snippet clamp upper bound             |
| `CHUNK_MIN_CHARS` | `16`    | Ignore ultra-short snippets if needed |

### Input path

* **JSONL**: `code_data/out/${PROJECT_NAME}/code_chunks.jsonl`
  (override via dedicated var if you expose one; otherwise this is the convention)

---

## 7) Operational notes

* **Fresh rebuild**: each indexing pass is destructive: droplet-safe for CI/CD (no drift).
* **Batching**: `QDRANT_BATCH_SIZE` controls both embedding and upsert batch sizes.
* **Backoff**: transient Qdrant/IO errors should be retried per batch.
* **Throughput**: increase `EMBEDDING_CONCURRENCY` with care (CPU bound).

---

## 8) Quickstart (conceptual)

1. Set env (see above). Minimal set:

   ```
   PROJECT_NAME=project_x
   QDRANT_URL=http://localhost:6334
   QDRANT_COLLECTION=mr_ai_code
   EMBEDDING_MODEL=bge-m3
   EMBEDDING_DIM=1024
   ```
2. Ensure Qdrant is running with gRPC on `6334`.
3. Run **index** command → the module drops & recreates `mr_ai_code` and ingests JSONL.
4. Run **search** command → returns **top-8** chunks (or set `RAG_TOP_K`).

---

## 9) Security & secrets

* **Never** commit real tokens (e.g., `GIT_TOKEN`); use CI/CD secrets.
* If you add remote embedding providers later, do not log raw code or prompts at info level.
* Prefer local embeddings (`bge-m3`) for privacy-sensitive repos.

---

## 10) FAQ

* **Top-7 vs Top-8?**
  This build uses `RAG_TOP_K` from env (current default **8**). Change env to 7 if you need exactly seven.

* **Cosine or Dot?**
  Keep **Cosine** for `bge-m3` unless you explicitly remap vector norms.

* **Why gRPC URL on 6334?**
  The official Rust Qdrant client uses gRPC (tonic). Point `QDRANT_URL` to the gRPC port.
