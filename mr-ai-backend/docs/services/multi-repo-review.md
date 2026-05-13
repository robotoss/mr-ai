# Cross-repo MR review (monorepo, multi-provider)

> **Status:** BETA · M1–M5 complete; awaiting load-test on a real multi-provider monorepo before promotion to STABLE.

One project may bundle N git repositories with declared
dependencies. Reviewing a merge request in any one of them should
pull cross-repo context — sibling repos' code, plus (case 3) the
linked MR's diff if its branch happens to exist on another repo.
The plan covers three user-visible scenarios:

1. **Branch only in packages-repo** — webhook → review pulls
   primary repo at `main` as context.
2. **Branch only in primary-repo** — webhook → review pulls
   packages at `main` as context.
3. **Parallel branches in both repos** — webhook → review pulls
   the sibling MR's head SHA and inlines its diff.

Each repo can live on a different provider (GitLab + GitHub +
Bitbucket coexist on the same instance), so M1 establishes the
cross-provider primitives before any review-pipeline change.

## Operator setup checklist

Before the cross-repo flow lights up, three things have to be in place.
Skipping any one of them is the most common reason "case 1/2 doesn't
work" — the review will still run for the repo that received the
webhook, just without sibling context.

1. **`projects.toml` declares every repo under one `[[project]]`.**
   Each repo entry needs `provider` + `remote_url` (+ optional
   `default_branch` and `is_primary`). See `projects.toml.example`.

2. **At least one `[[project.dependency]]` edge per repo pair.** The
   overlay BFS walker reaches siblings by following these edges. It
   walks **both directions**, so one edge `app → packages` is enough
   to cover webhooks from either side. Without any edge, overlay sees
   only the primary repo. _Case 3 (parallel branches) still works
   without edges — discovery iterates every repo in the project — but
   case 1+2 do not._

3. **Per-provider HMAC secrets + per-host tokens.** Set
   `GITLAB_WEBHOOK_SECRET` / `GITHUB_WEBHOOK_SECRET` /
   `BITBUCKET_WEBHOOK_SECRET` for the providers you receive webhooks
   from, and `GIT_TOKEN_<HOST_SLUG>` for every host whose API the
   worker calls (primary + every sibling). Missing host tokens fail
   silently per sibling with a `warn!(target="cross_repo.discover")`
   line — check logs if discovery comes back empty unexpectedly.

## Foundation (sprint M1)

### Per-host API base URL

`secrets::base_api_for(host, ProviderKind)` resolves a canonical
API base URL with the same host-slug pattern already used for
tokens:

| Lookup | Wins when |
|---|---|
| `GIT_API_BASE_<HOST_SLUG>` env | Self-hosted instance, operator sets the override |
| Provider default | Public clouds; zero-config |

Defaults:

| Provider | Host | Returned base API |
|---|---|---|
| GitLab | `gitlab.com` | `https://gitlab.com/api/v4` |
| GitLab | self-hosted | `https://<host>/api/v4` |
| GitHub | `github.com` | `https://api.github.com` |
| GitHub | GHE self-hosted | `https://<host>/api/v3` |
| Bitbucket | `bitbucket.org` | `https://api.bitbucket.org/2.0` |

Worker `IngestMrHandler::build_provider_ctx` now derives
`ProviderConfig.base_api` per repo via this helper — the legacy
`DefaultRegistryConfig.git_api_base` field still exists as a
fallback for hosts that fail to parse, but routes/worker no
longer rely on it.

### Per-provider webhook secrets (hard cut)

`SecretKey::WebhookHmac` (one global secret for all three
schemes) is **removed**. Three new variants land:

| Variant | Env var | File path |
|---|---|---|
| `WebhookHmacGitlab` | `GITLAB_WEBHOOK_SECRET` | `<root>/_global/webhook_hmac_gitlab` |
| `WebhookHmacGithub` | `GITHUB_WEBHOOK_SECRET` | `<root>/_global/webhook_hmac_github` |
| `WebhookHmacBitbucket` | `BITBUCKET_WEBHOOK_SECRET` | `<root>/_global/webhook_hmac_bitbucket` |

Each webhook handler resolves its own. No global fallback —
this is intentional so a leak of one provider's secret never
hands an attacker the other providers' endpoints.

**Operators must update env / mounted-file layouts before
deploying M1.** A deployment with only the legacy
`WEBHOOK_HMAC_SECRET` set returns `503 WEBHOOK_SECRET_UNSET`
on the first request. See [operations](../operations.md) for
the migration checklist.

## Roadmap

| Sprint | What |
|---|---|
| **M1 ✅** (5ce58c1) | `base_api_for` helper, per-provider webhook secrets, per-repo `ProviderConfig`. |
| **M2 ✅** (cf4b9e6) | `build_two_phase_review` accepts `Option<&OverlayEmbedCache>`. Worker builds the overlay (`build_for_mr`) and the embed cache before invoking the review. RAG builders merge top-3 sibling-repo chunks per target via cosine similarity. Failure to build the overlay degrades to legacy single-repo review. Cases 1 + 2 live. |
| **M3 ✅** (1992158) | `ProviderClient::list_open_mrs_by_branch(project, source_branch) -> Vec<MrSummary>` on all three providers (GitLab `/merge_requests?source_branch=...`, GitHub `/pulls?head=<owner>:<branch>`, Bitbucket BBQL `q=source.branch.name=...`). 6 wiremock tests in `tests/discovery_api.rs` exercise empty results, fork-prefix passthrough, provider dispatch. |
| **M4 ✅** (3d70f7e) | Worker `discover_linked_mrs` stage: per-sibling `list_open_mrs_by_branch` → most-recent-wins picker → `fetch_bundle` → flatten `raw_unidiff`. `build_for_mr` gains `head_overrides: &HashMap<RepoId, String>` so sibling repos with a parallel MR check out at the linked head (case 3). `build_two_phase_review` + prompt builder gain `linked_mrs: &[LinkedMrDiff]` — each target's prompt now embeds a `LINKED_MR_DIFFS` (READ-ONLY, NON-AUTHORITATIVE) block listing provider, repo slug, IID, head SHA, and the joined diff. Failures degrade to "skip this sibling" without blocking the review. |
| **M5 ✅** (2c72cce) | Docs polish: cross-repo MR sequence diagram in `architecture/data-flow.md`, env-knob row in `operations.md` flags M4 discovery semantics, README TOC reflects the BETA promotion. Testcontainer integration scaffolding deferred — the unit coverage from M2–M4 (overlay merge, prompt-builder section, picker policy, head-ref resolver) plus the 6 wiremock provider tests pin the moving parts; a real-monorepo load test is the next gate, tracked in `Out of scope`. |

### Case 3: parallel branches in both repos

When the webhook arrives for a branch like `feat/x`, the worker runs
[`discover_linked_mrs`](../../worker/src/handlers/ingest_mr/stages.rs) once:

1. List every sibling repo under the same project (`projects::list_repos_for_project`).
2. For each, build a per-host `ProviderClient` (using M1's `base_api_for`
   helper so a sibling on a different provider works transparently)
   and call `list_open_mrs_by_branch(slug, "feat/x")`.
3. Empty → skip that sibling. Multiple → pick the one with the
   newest `updated_at` and emit `warn!(target="cross_repo.ambiguous")`.
4. Fetch the chosen MR's bundle, flatten `FileChange.raw_unidiff` into
   a single string.

The output feeds both the overlay (`head_overrides` pins the sibling
checkout to the linked head SHA) and the prompt:

```text
=== LINKED_MR_DIFFS (READ-ONLY, NON-AUTHORITATIVE) ===
The following diffs come from sibling repositories whose
branch name matches this MR. Use them only to understand
how the change interacts with the rest of the project.
You MUST NOT raise issues against lines from these diffs.

--- linked from GitHub:acme/packages#7 (branch feat/x)
URL: https://github.com/acme/packages/pull/7
HEAD_SHA: deadbeef

<joined raw_unidiff body>
---
```

A failed bundle fetch keeps the head_overrides entry (so the overlay
still pins to the right SHA) and emits a metadata-only footer in the
prompt — the reviewer LLM still sees that a sibling MR exists.

## Acceptance

After M5 every claim below holds on `cargo test --workspace`:

- A `projects.toml` may federate repos across GitLab + GitHub +
  Bitbucket inside one `[[project]]`. Each `[[project.repo]]`
  declares its `provider` independently; tokens and HMAC secrets
  resolve per-host / per-provider (M1).
- Webhooks signed by GitLab and webhooks signed by GitHub hitting
  the same instance verify against **separate** HMAC secrets
  (`GITLAB_WEBHOOK_SECRET` vs `GITHUB_WEBHOOK_SECRET`); a leak of
  one never accepts the others' payloads (M1).
- Case 1 auto: webhook from packages-repo branch `feat/x` → worker
  pulls app-repo at `main` into review context (M2).
- Case 2 auto: webhook from app-repo branch `feat/x` → worker
  pulls packages-repo at `main` into review context (M2).
- Case 3 auto: webhook from app-repo `feat/x` → worker discovers
  open MR on packages-repo `feat/x` via `list_open_mrs_by_branch`
  → overlay pinned to packages `head_sha`, prompt embeds the
  linked MR's diff in a `LINKED_MR_DIFFS` block (M3 + M4).
- Zero linked MRs and multiple linked MRs (`warn!` +
  most-recent-wins) both produce a successful review (M4).
- Every per-sibling failure (provider down, token missing, bundle
  fetch error) degrades to skipping that sibling, never to a
  failed review (M2 + M4).

## Out of scope

- Linked-MR discovery via PR/MR title parsing.
- Persistent `mr_links` cache (discovery on every webhook is
  cheap enough — N-1 API calls per project).
- Auto-detection of `[[project.dependency]]` edges by parsing
  `package.json` / `Cargo.toml` / `pubspec.yaml`.
- Auto-opening a linked MR when reviewer suggests it.
- Full-stack testcontainer suite for the cross-repo cases. Coverage
  today is unit-level: pure picker / ref-resolver / prompt-section
  tests plus 6 wiremock provider tests. The first real monorepo
  rollout (BETA gate) covers the end-to-end path against live
  providers.

## Related docs

- [secrets guide](../guides/secrets.md) — host-scoped overrides.
- [operations](../operations.md) — multi-tenant + multi-provider
  deploy checklist.
- [review-pipeline](review-pipeline.md) — worker flow.
- [git-context-engine](git-context-engine.md) — review building.
