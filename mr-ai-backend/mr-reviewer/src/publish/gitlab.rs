//! GitLab publisher (step 5).
//!
//! Uses Discussions API for inline comments and MR Notes for file/global.
//!
//! API:
//! - POST /projects/:id/merge_requests/:iid/discussions   (inline)
//! - POST /projects/:id/merge_requests/:iid/notes         (general)
//! - GET  /projects/:id/merge_requests/:iid/discussions   (for idempotency)
//! - GET  /projects/:id/merge_requests/:iid/notes         (for idempotency, fallback)
//!
//! Position requires `head_sha` + `base_sha` + (usually) `start_sha` from MR meta.
//!
//! Notable fixes & improvements:
//! - URL-encodes `project` segments in all endpoints.
//! - Posts the full markdown body and appends a hidden idempotency marker.
//! - Loads existing markers from both discussions and notes.
//! - Supports both `new_*` and `old_*` inline positions (with auto-retry).
//! - Passes `start_sha` when available.
//! - Applies robust HTTP timeouts and limited concurrency.
//! - Retries transient errors (5xx/429) with exponential backoff honoring `Retry-After`.

use std::{collections::HashSet, sync::Arc, time::Duration};

use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::errors::{Error, MrResult};
use crate::git_providers::ChangeRequestId;
use crate::map::TargetRef;
use crate::review::DraftComment;
use crate::{
    ReviewPlan,
    publish::{ProviderIds, PublishConfig, PublishedComment},
};
use urlencoding::encode;

/// Hidden marker we embed into comment body to detect duplicates.
/// Example: `<!-- mrai:key=packages/a.dart:42|line;hash=abcdef;ver=1 -->`
const _MARKER_PREFIX: &str = "<!-- mrai:key="; // kept for clarity

/// Maximum attempts for transient failures (HTTP 5xx / 429).
const MAX_RETRIES: usize = 3;

/// Initial backoff for transient failures.
const INITIAL_BACKOFF_MS: u64 = 400;

/// Publish all drafts to GitLab.
///
/// Loads existing markers (from both discussions and notes) to enforce idempotency,
/// then publishes each draft with bounded concurrency and robust error handling.
///
/// # Parameters
/// - `cfg`: Provider configuration (token, base API).
/// - `id`: MR identifier (project path or id, IID).
/// - `plan`: Review plan (used for MR diff refs).
/// - `drafts`: Draft comments to publish.
/// - `pcfg`: Publish configuration (dry-run, concurrency, etc.).
///
/// # Returns
/// List of `PublishedComment` describing what was performed or skipped.
pub async fn publish_gitlab(
    cfg: &crate::git_providers::ProviderConfig,
    id: &ChangeRequestId,
    plan: &ReviewPlan,
    drafts: &[DraftComment],
    pcfg: &PublishConfig,
) -> MrResult<Vec<PublishedComment>> {
    let http = build_http_client()?;
    let headers = build_gitlab_headers(&cfg.token)?;
    let base = cfg.base_api.trim_end_matches('/');

    // Load existing markers to enforce idempotency (from discussions and notes)
    let existing_disc = load_existing_markers_from_discussions(&http, &headers, base, id).await?;
    let existing_notes = load_existing_markers_from_notes(&http, &headers, base, id).await?;
    let existing = existing_disc
        .union(&existing_notes)
        .cloned()
        .collect::<HashSet<_>>();
    info!(
        "step5: existing markers discussions={} notes={} union={}",
        existing_disc.len(),
        existing_notes.len(),
        existing.len()
    );

    // Extract SHAs for inline comment positions (pass start_sha when available)
    let head = plan.bundle.meta.diff_refs.head_sha.clone();
    let base_sha = plan.bundle.meta.diff_refs.base_sha.clone();
    let start_sha_opt = plan.bundle.meta.diff_refs.start_sha.clone();

    // Concurrency guard
    let sem = Arc::new(Semaphore::new(pcfg.max_concurrency.max(1)));

    let mut futs = Vec::with_capacity(drafts.len());
    for d in drafts.iter().cloned() {
        let http = http.clone();
        let headers = headers.clone();
        let base = base.to_string();
        let id = id.clone();
        let head = head.clone();
        let base_sha = base_sha.clone();
        let start_sha_opt = start_sha_opt.clone();
        let dry_run = pcfg.dry_run;
        let allow_edit = pcfg.allow_edit;
        let existing = existing.clone();
        let sem_cloned = sem.clone();

        futs.push(tokio::spawn(async move {
            let _permit = sem_cloned.acquire_owned().await.unwrap();
            publish_one(
                &http,
                &headers,
                &base,
                &id,
                &d,
                &head,
                &base_sha,
                start_sha_opt.as_deref(),
                dry_run,
                allow_edit,
                &existing,
            )
            .await
        }));
    }

    let mut out = Vec::with_capacity(drafts.len());
    for f in futs {
        out.push(
            f.await
                .map_err(|e| Error::Validation(format!("join error: {e}")))??,
        );
    }
    Ok(out)
}

/// Publish one draft, respecting idempotency and dry-run.
///
/// Decides between inline and general note. Inline posting first tries `new_*` side,
/// and on characteristic `line_code` validation errors automatically retries as `old_*`.
async fn publish_one(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    id: &ChangeRequestId,
    draft: &DraftComment,
    head_sha: &str,
    base_sha: &str,
    start_sha_opt: Option<&str>,
    dry_run: bool,
    _allow_edit: bool,
    existing: &HashSet<String>,
) -> MrResult<PublishedComment> {
    let (marker, key, _) = make_marker_and_key(draft);

    let body = if draft.body_markdown.trim().is_empty() {
        format!("Review note\n\n{}", marker)
    } else {
        format!("{}\n\n{}", draft.body_markdown.trim(), marker)
    };

    // Idempotency: skip if key is present
    if existing.contains(&key) {
        debug!("step5: skip duplicate key={}", key);
        return Ok(PublishedComment {
            target: draft.target.clone(),
            performed: false,
            created_new: false,
            skipped_reason: Some("duplicate".into()),
            provider_ids: None,
        });
    }

    // Inline or general?
    match &draft.target {
        TargetRef::Line { path, line } => {
            publish_inline(
                http,
                headers,
                base_api,
                id,
                path,
                *line,
                body,
                head_sha,
                base_sha,
                start_sha_opt,
                dry_run,
            )
            .await
        }
        TargetRef::Range {
            path, start_line, ..
        } => {
            publish_inline(
                http,
                headers,
                base_api,
                id,
                path,
                *start_line,
                body,
                head_sha,
                base_sha,
                start_sha_opt,
                dry_run,
            )
            .await
        }
        TargetRef::Symbol {
            path, decl_line, ..
        } => {
            publish_inline(
                http,
                headers,
                base_api,
                id,
                path,
                *decl_line,
                body,
                head_sha,
                base_sha,
                start_sha_opt,
                dry_run,
            )
            .await
        }
        TargetRef::File { .. } | TargetRef::Global => {
            publish_general(http, headers, base_api, id, body, dry_run).await
        }
    }
}

/// Construct inline discussion and POST to GitLab with robust behavior.
///
/// Strategy:
/// 1) Attempt as `new_*` side (line exists in head).
/// 2) If GitLab rejects with a line_code-like error, retry as `old_*` side (line exists in base).
///
/// If both attempts fail with validation that implies an invalid position, consider
/// falling back to a general note at the caller, if desired.
async fn publish_inline(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    id: &ChangeRequestId,
    path: &str,
    line: usize,
    body: String,
    head_sha: &str,
    base_sha: &str,
    start_sha_opt: Option<&str>,
    dry_run: bool,
) -> MrResult<PublishedComment> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/discussions",
        base_api,
        encode(&id.project),
        id.iid
    );

    #[derive(serde::Serialize)]
    struct Position<'a> {
        /// Must be "text" for textual diffs.
        position_type: &'a str,
        /// Old (base) side. Provide either the old_* pair or the new_* pair.
        #[serde(skip_serializing_if = "Option::is_none")]
        old_path: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_line: Option<usize>,
        /// New (head) side. Provide either the old_* pair or the new_* pair.
        #[serde(skip_serializing_if = "Option::is_none")]
        new_path: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_line: Option<usize>,
        /// MR diff refs.
        head_sha: &'a str,
        base_sha: &'a str,
        /// Some GitLab versions require start_sha for valid positions.
        #[serde(skip_serializing_if = "Option::is_none")]
        start_sha: Option<&'a str>,
    }

    #[derive(serde::Serialize)]
    struct Req<'a> {
        body: &'a str,
        position: Position<'a>,
    }

    // GitLab expects 1-based line numbers.
    let line_1b = line.max(1);

    debug!(
        "step5: inline POST path={} line={} (1b={}) dry_run={}",
        path, line, line_1b, dry_run
    );

    if dry_run {
        return Ok(PublishedComment {
            target: TargetRef::Line {
                path: path.to_string(),
                line: line_1b,
            },
            performed: false,
            created_new: true,
            skipped_reason: Some("dry-run".into()),
            provider_ids: None,
        });
    }

    // 1) Try as new_* side.
    let req_new = Req {
        body: &body,
        position: Position {
            position_type: "text",
            old_path: None,
            old_line: None,
            new_path: Some(path),
            new_line: Some(line_1b),
            head_sha,
            base_sha,
            start_sha: start_sha_opt,
        },
    };

    match post_with_retries(http, headers, &url, &req_new).await {
        Ok(resp) => {
            #[derive(serde::Deserialize)]
            struct DiscussionResp {
                id: String,
            }
            let disc: DiscussionResp = resp
                .json()
                .await
                .unwrap_or(DiscussionResp { id: String::new() });
            return Ok(PublishedComment {
                target: TargetRef::Line {
                    path: path.to_string(),
                    line: line_1b,
                },
                performed: true,
                created_new: true,
                skipped_reason: None,
                provider_ids: Some(ProviderIds {
                    discussion_id: Some(disc.id),
                    note_id: None,
                }),
            });
        }
        Err(Error::Validation(msg)) => {
            // Characteristic GitLab validation for invalid positions returns an error mentioning line_code.
            let should_retry_old = looks_like_line_code_error(&msg);
            if !should_retry_old {
                return Err(Error::Validation(msg));
            }
            warn!("step5: retry inline as old_* due to validation: {}", msg);
        }
        Err(e) => return Err(e),
    }

    // 2) Retry as old_* side (removed/modified lines on base).
    let req_old = Req {
        body: &body,
        position: Position {
            position_type: "text",
            old_path: Some(path),
            old_line: Some(line_1b),
            new_path: None,
            new_line: None,
            head_sha,
            base_sha,
            start_sha: start_sha_opt,
        },
    };

    let resp = post_with_retries(http, headers, &url, &req_old).await?;
    #[derive(serde::Deserialize)]
    struct DiscussionResp {
        id: String,
    }
    let disc: DiscussionResp = resp
        .json()
        .await
        .unwrap_or(DiscussionResp { id: String::new() });

    Ok(PublishedComment {
        target: TargetRef::Line {
            path: path.to_string(),
            line: line_1b,
        },
        performed: true,
        created_new: true,
        skipped_reason: None,
        provider_ids: Some(ProviderIds {
            discussion_id: Some(disc.id),
            note_id: None,
        }),
    })
}

/// Create a general MR note (file/global).
async fn publish_general(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    id: &ChangeRequestId,
    body: String,
    dry_run: bool,
) -> MrResult<PublishedComment> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/notes",
        base_api,
        encode(&id.project),
        id.iid
    );

    #[derive(serde::Serialize)]
    struct Req<'a> {
        body: &'a str,
    }
    debug!("step5: note POST dry_run={}", dry_run);

    if dry_run {
        return Ok(PublishedComment {
            target: TargetRef::Global,
            performed: false,
            created_new: true,
            skipped_reason: Some("dry-run".into()),
            provider_ids: None,
        });
    }

    let resp = post_with_retries(http, headers, &url, &Req { body: &body }).await?;

    #[derive(serde::Deserialize)]
    struct NoteResp {
        id: u64,
    }
    let nr: NoteResp = resp.json().await.unwrap_or(NoteResp { id: 0 });

    Ok(PublishedComment {
        target: TargetRef::Global,
        performed: true,
        created_new: true,
        skipped_reason: None,
        provider_ids: Some(ProviderIds {
            discussion_id: None,
            note_id: Some(nr.id),
        }),
    })
}

/// Load existing discussion bodies and extract mrai markers for idempotency.
async fn load_existing_markers_from_discussions(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    id: &ChangeRequestId,
) -> MrResult<HashSet<String>> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/discussions?per_page=100",
        base_api,
        encode(&id.project),
        id.iid
    );
    #[derive(serde::Deserialize)]
    struct Note {
        body: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Discussion {
        notes: Vec<Note>,
    }

    let resp = get_with_retries(http, headers, &url).await?;
    let discussions: Vec<Discussion> = resp.json().await.unwrap_or_default();
    Ok(extract_markers_from_bodies(
        discussions
            .into_iter()
            .flat_map(|d| d.notes.into_iter().filter_map(|n| n.body))
            .collect(),
    ))
}

/// Load existing MR notes and extract mrai markers (complements discussions).
async fn load_existing_markers_from_notes(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    id: &ChangeRequestId,
) -> MrResult<HashSet<String>> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/notes?per_page=100",
        base_api,
        encode(&id.project),
        id.iid
    );
    #[derive(serde::Deserialize)]
    struct Note {
        body: Option<String>,
    }

    let resp = get_with_retries(http, headers, &url).await?;
    let notes: Vec<Note> = resp.json().await.unwrap_or_default();
    Ok(extract_markers_from_bodies(
        notes.into_iter().filter_map(|n| n.body).collect(),
    ))
}

/// Extract idempotency markers from a list of HTML/Markdown bodies.
///
/// Marker format: `<!-- mrai:key=<key>;hash=<hex>;ver=<int> -->`
///
/// Returns a set of `<key>#<hash>` strings used for duplicate detection.
fn extract_markers_from_bodies(bodies: Vec<String>) -> HashSet<String> {
    let mut set = HashSet::new();
    let re = Regex::new(r"<!--\s*mrai:key=([^;>]+);hash=([0-9a-f]+);ver=\d+\s*-->").unwrap();
    for b in bodies {
        if let Some(caps) = re.captures(&b) {
            let key = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
            let hash = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
            set.insert(format!("{}#{}", key, hash));
        }
    }
    set
}

/// Build the idempotency key and marker string for a draft.
///
/// Key format: `<path>:<line_or_decl_or_start>|<kind>`
/// - File/Global use "file" or "global".
fn make_marker_and_key(d: &DraftComment) -> (String, String, Option<usize>) {
    let (path, line_opt, kind) = match &d.target {
        TargetRef::Line { path, line } => (path.clone(), Some(*line), "line"),
        TargetRef::Range {
            path, start_line, ..
        } => (path.clone(), Some(*start_line), "range"),
        TargetRef::Symbol {
            path, decl_line, ..
        } => (path.clone(), Some(*decl_line), "symbol"),
        TargetRef::File { path } => (path.clone(), None, "file"),
        TargetRef::Global => ("".to_string(), None, "global"),
    };

    let line_key = line_opt
        .map(|l| l.to_string())
        .unwrap_or_else(|| "-".into());
    let key = format!("{}:{}|{}", path, line_key, kind);
    let full_key = format!("{}#{}", key, d.snippet_hash);

    let marker = format!("<!-- mrai:key={};hash={};ver=1 -->", key, d.snippet_hash);

    (marker, full_key, line_opt)
}

/// Build a tuned HTTP client with sane timeouts and pooling.
fn build_http_client() -> MrResult<reqwest::Client> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .pool_idle_timeout(Some(Duration::from_secs(90)))
        .pool_max_idle_per_host(8)
        .build()?;
    Ok(client)
}

/// Build GitLab headers, including Private Token.
fn build_gitlab_headers(token: &str) -> MrResult<HeaderMap> {
    let mut h = HeaderMap::new();
    h.insert(USER_AGENT, HeaderValue::from_static("mr-reviewer/1.0"));
    h.insert(ACCEPT, HeaderValue::from_static("application/json"));
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h.insert(
        "PRIVATE-TOKEN",
        HeaderValue::from_str(token).map_err(|e| Error::Validation(format!("bad token: {e}")))?,
    );
    Ok(h)
}

/// Returns true if the given error message looks like a GitLab invalid `line_code` validation.
fn looks_like_line_code_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("line_code")
        || m.contains("must be a valid line code")
        || m.contains("can't be blank")
}

/// POST with retries for transient failures; returns non-success as Validation error.
///
/// - Retries on 429/5xx with exponential backoff.
/// - Honors `Retry-After` header when present.
/// - For non-retriable statuses, returns `Validation` to bubble up API details.
async fn post_with_retries<T: serde::Serialize>(
    http: &reqwest::Client,
    headers: &HeaderMap,
    url: &str,
    body: &T,
) -> MrResult<reqwest::Response> {
    request_with_retries(http, headers, |c| c.post(url).json(body)).await
}

/// GET with retries for transient failures.
async fn get_with_retries(
    http: &reqwest::Client,
    headers: &HeaderMap,
    url: &str,
) -> MrResult<reqwest::Response> {
    request_with_retries(http, headers, |c| c.get(url)).await
}

/// Shared retry helper for reqwest requests.
///
/// Accepts a closure that builds a `RequestBuilder` (e.g., POST with JSON or GET),
/// executes it with retries on 429/5xx, and returns the final `Response` on success.
async fn request_with_retries(
    http: &reqwest::Client,
    headers: &HeaderMap,
    mut build: impl FnMut(&reqwest::Client) -> reqwest::RequestBuilder,
) -> MrResult<reqwest::Response> {
    let mut attempt = 0;
    let mut backoff_ms = INITIAL_BACKOFF_MS;

    loop {
        attempt += 1;
        let req = build(http).headers(headers.clone());
        let resp = req.send().await;

        match resp {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) => {
                let status = r.status();
                // Clone headers before consuming the response body
                let headers_snapshot = r.headers().clone();

                // Body can only be consumed once
                let body = r.text().await.ok();

                if status.as_u16() == 429 || status.is_server_error() {
                    if attempt >= MAX_RETRIES {
                        return Err(Error::Validation(format!(
                            "gitlab request failed after retries: status={} body={:?}",
                            status, body
                        )));
                    }

                    // Use header snapshot (safe even after consuming body)
                    let retry_after_ms = headers_snapshot
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|secs| secs * 1_000);

                    let sleep_ms = retry_after_ms.unwrap_or(backoff_ms);
                    warn!(
                        "gitlab transient status={} attempt={}/{} backoff={}ms body={:?}",
                        status, attempt, MAX_RETRIES, sleep_ms, body
                    );
                    tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                    backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
                    continue;
                }

                return Err(Error::Validation(format!(
                    "gitlab request failed: status={} body={:?}",
                    status, body
                )));
            }
            Err(e) => {
                if attempt >= MAX_RETRIES {
                    return Err(Error::Other(format!(
                        "gitlab network error after retries: {e}"
                    )));
                }
                tracing::warn!(
                    "gitlab network error attempt={}/{} backoff={}ms err={}",
                    attempt,
                    MAX_RETRIES,
                    backoff_ms,
                    e
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
            }
        }
    }
}
