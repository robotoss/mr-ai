# Configuration Reference

All runtime configuration lives in `.env` at the workspace root. Every
variable is read **once at startup** by either `GatewayConfig::from_env()`
or one of the layer-specific config loaders. Defaults documented below
match the behaviour in code at the time of writing — if they drift, treat
the code as truth and update this page.

> The shipped [`.env.example`](../../.env.example) is always a working
> reference. Copy it to `.env` and edit values.

## Logging

| Var | Default | Purpose |
| --- | --- | --- |
| `LOG_LEVEL` | `info` | `EnvFilter` expression. Examples: `debug`, `info,reqwest=warn`. |
| `LOG_DIR` | `logs` | Directory for daily-rotated JSON log files. Created if missing. |
| `LOG_FILE_PREFIX` | `mr-ai` | Filename prefix; full file is `<dir>/<prefix>.YYYY-MM-DD`. |

Loaded by [`init_tracing`](../../ai-llm-service/src/telemetry.rs).

## Usage history

Append-only per-call audit log. See
[reference/usage-log](../reference/usage-log.md) for the full schema and
`jq` cookbook.

| Var | Default | Purpose |
| --- | --- | --- |
| `USAGE_LOG_PATH` | `logs/usage.jsonl` | JSONL file; one record per gateway call. |
| `USAGE_LOG_DISABLED` | `false` | Replace persister with a no-op. Counters still run. |
| `USAGE_LOG_INCLUDE_PROMPTS` | `false` | Add truncated prompt/response previews to records. |
| `USAGE_LOG_PREVIEW_CHARS` | `200` | Preview truncation length (Unicode chars). |

## Gateway

| Var | Default | Purpose |
| --- | --- | --- |
| `LLM_PRICING_PATH` | `pricing.toml` | Path to the pricing table (workspace-relative ok). |
| `LLM_HEALTH_TIMEOUT_SECS` | `10` | Reserved; per-provider HTTP client timeout takes precedence. |

## Per-tier provider config

For each `<TIER>` ∈ `FAST`, `SMART`, `EMBED`:

| Var | Required | Notes |
| --- | --- | --- |
| `LLM_<TIER>_PROVIDER` | yes | `ollama` / `openai` / `bedrock`. Synonyms accepted (e.g. `local`, `chatgpt`, `aws`). |
| `LLM_<TIER>_MODEL` | yes | Provider-specific model id. |
| `LLM_<TIER>_ENDPOINT` | no | Falls back to provider-specific default. |
| `LLM_<TIER>_API_KEY` | varies | Falls back to provider-specific shared key. |
| `LLM_<TIER>_MAX_TOKENS` | no | u32. |
| `LLM_<TIER>_TEMPERATURE` | no | f32. |
| `LLM_<TIER>_TOP_P` | no | f32. |
| `LLM_<TIER>_TIMEOUT_SECS` | no | HTTP client timeout. |

### Provider-specific overrides

| Var | Used for | Default |
| --- | --- | --- |
| `LLM_<TIER>_REGION` | Bedrock signing region. | First non-empty of `AWS_REGION`, `AWS_DEFAULT_REGION`, then `us-east-1`. |
| `LLM_<TIER>_SECRET_KEY` | Bedrock secret access key. | Fallback `AWS_SECRET_ACCESS_KEY`. |
| `LLM_<TIER>_SESSION_TOKEN` | Bedrock STS session token. | Fallback `AWS_SESSION_TOKEN`. |

### Provider-shared fallbacks

Used when the per-tier `_ENDPOINT` / `_API_KEY` is unset.

| Var | Used by |
| --- | --- |
| `OLLAMA_URL` | Ollama (default `http://localhost:11434`). |
| `OPENAI_BASE_URL` | OpenAI (default `https://api.openai.com`). |
| `OPENAI_API_KEY` | OpenAI. |
| `AWS_REGION` / `AWS_DEFAULT_REGION` | Bedrock. |
| `AWS_ACCESS_KEY_ID` | Bedrock access key. |
| `AWS_SECRET_ACCESS_KEY` | Bedrock secret key. |
| `AWS_SESSION_TOKEN` | Bedrock STS session token (optional). |

Endpoint resolution order (per tier):
1. `LLM_<TIER>_ENDPOINT` if set.
2. Provider default — `OLLAMA_URL` / `OPENAI_BASE_URL` /
   `https://bedrock-runtime.<region>.amazonaws.com`.

## RAG / Qdrant

Loaded by [`RagConfig::from_env`](../../rag-base/src/structs/rag_base_config.rs).

| Var | Default | Purpose |
| --- | --- | --- |
| `QDRANT_URL` | `http://localhost:6334` | gRPC endpoint. |
| `QDRANT_COLLECTION` | `mr_ai_code` | Collection name. |
| `QDRANT_DISTANCE` | `Cosine` | One of `Cosine`, `Dot`, `Euclid`. |
| `QDRANT_BATCH_SIZE` | `256` | Upsert batch size. |
| `EMBEDDING_DIM` | `1024` | Strict invariant on vectors returned by gateway. Must match the gateway's embedding model. |
| `RAG_DISABLE` | `false` | Short-circuit search to empty. |
| `RAG_TOP_K` | `20` | Default `k`. |
| `RAG_MIN_SCORE` | `0.0` | Minimum vector score. |
| `RAG_TAKE_PER_TARGET` | unset | Cap when aggregating by target. |
| `RAG_MEMO_CAP` | unset | Optional memoisation capacity. |
| `INDEX_JSONL_PATH` | `code_data/out/<project>/code_chunks.jsonl` | Override input path. |
| `CLAMP_PREVIEW_MAX_CHARS` | `320` | Preview snippet budget. |
| `CLAMP_PREVIEW_MAX_LINES` | `50` | Preview snippet line cap. |
| `CLAMP_EMBED_MAX_CHARS` | `1200` | Embedding text budget (chars). |
| `CLAMP_EMBED_MAX_LINES` | `80` | Embedding text budget (lines). |
| `CHUNK_MIN_CHARS` | `16` | Minimum chunk size to retain. |

## API server

Loaded by [`AppConfig::from_env`](../../api/src/core/app_state.rs).

| Var | Required | Purpose |
| --- | --- | --- |
| `API_ADDRESS` | yes | Bind address (e.g. `0.0.0.0:8080`). |
| `PROJECT_NAME` | yes | Logical project key. |
| `GIT_API_BASE` | yes | Git provider base URL (must be `http(s)`). |
| `GIT_TOKEN` | yes | Git provider token. |
| `TRIGGER_SECRET` | yes | Shared secret guarding `/trigger_git_mr`. |

## Git cloning

Loaded by [`project-code-store`](../services/project-code-store.md).

| Var | Purpose |
| --- | --- |
| `SSH_KEY_PATH` | Absolute path to private key; falls back to ssh-agent. |
| `GIT_HTTP_TOKEN` | HTTPS auth token. |
| `GIT_HTTP_USER` | HTTPS username (default `oauth2`). |

## Configuration patterns

- **Ollama-only smoke**: set the three tiers to Ollama and pull the
  matching models.
- **Mixed setup**: Smart tier on Bedrock Claude (deep reviews), Fast tier
  on Ollama (cheap parsing), Embed on Ollama bge-m3.
- **Cloud-only**: Smart on OpenAI `gpt-4o`, Fast on `gpt-4o-mini`, Embed on
  `text-embedding-3-small`.

## Related docs

- [Getting Started](getting-started.md)
- [Add a new LLM Provider](add-llm-provider.md)
- [Observability](observability.md)
- [Reference: Pricing](../reference/pricing.md)
