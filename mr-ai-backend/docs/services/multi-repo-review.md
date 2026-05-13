# Cross-repo MR review (monorepo, multi-provider)

> **Status:** IN PROGRESS · Sprint M1 of 5 in flight.

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
| **M2 ✅** (this commit) | `build_two_phase_review` accepts `Option<&OverlayEmbedCache>`. Worker builds the overlay (`build_for_mr`) and the embed cache before invoking the review. RAG builders merge top-3 sibling-repo chunks per target via cosine similarity. Failure to build the overlay degrades to legacy single-repo review. Cases 1 + 2 live. |
| M3 | `ProviderClient::list_open_mrs_by_branch` on GitLab + GitHub + Bitbucket. Wiremock tests per provider. |
| M4 | Worker discovery step + `build_for_mr(..., head_overrides)`. Prompt embeds `LINKED_MR_DIFFS` block. Case 3 lights up. |
| M5 | 2 testcontainer integration tests + docs polish. |

## Out of scope

- Linked-MR discovery via PR/MR title parsing.
- Persistent `mr_links` cache (discovery on every webhook is
  cheap enough — N-1 API calls per project).
- Auto-detection of `[[project.dependency]]` edges by parsing
  `package.json` / `Cargo.toml` / `pubspec.yaml`.
- Auto-opening a linked MR when reviewer suggests it.

## Related docs

- [secrets guide](../guides/secrets.md) — host-scoped overrides.
- [operations](../operations.md) — multi-tenant + multi-provider
  deploy checklist.
- [review-pipeline](review-pipeline.md) — worker flow.
- [git-context-engine](git-context-engine.md) — review building.
