//! GitLab publisher (step 5).
//!
//! Uses Discussions API for inline comments and MR Notes for file/global.
//!
//! API:
//! - POST /projects/:id/merge_requests/:iid/discussions   (inline)
//! - POST /projects/:id/merge_requests/:iid/notes         (general)
//! - GET  /projects/:id/merge_requests/:iid/discussions   (for idempotency)
//!
//! Position requires `head_sha` + `base_sha` + `start_sha` from MR meta.

use std::{collections::HashSet, sync::Arc, time::Duration};

use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use tokio::sync::Semaphore;
use tracing::{debug, info};

use crate::errors::{Error, MrResult};
use crate::git_providers::{ChangeRequestId, ProviderConfig};
use crate::map::TargetRef;
use crate::review::DraftComment;
use crate::{
    ReviewPlan,
    publish::{ProviderIds, PublishConfig, PublishedComment},
};

/// Hidden marker we embed into comment body to detect duplicates.
/// Example: `<!-- mrai:key=packages/a.dart:42;hash=abcdef;ver=1 -->`
const _MARKER_PREFIX: &str = "<!-- mrai:key="; // kept for clarity, currently not used directly

/// Publish all drafts to GitLab.
pub async fn publish_gitlab(
    cfg: &ProviderConfig,
    id: &ChangeRequestId,
    plan: &ReviewPlan,
    drafts: &[DraftComment],
    pcfg: &PublishConfig,
) -> MrResult<Vec<PublishedComment>> {
    // Prepare HTTP client
    let http = build_http_client()?;
    let headers = build_gitlab_headers(&cfg.token)?;
    let base = cfg.base_api.trim_end_matches('/');

    // Load existing markers to enforce idempotency
    let existing = load_existing_markers(&http, &headers, base, id).await?;
    info!("step5: existing markers={}", existing.len());

    // Extract SHAs for inline comment positions
    let head = plan.bundle.meta.diff_refs.head_sha.clone();
    let base_sha = plan.bundle.meta.diff_refs.base_sha.clone();
    let start_sha = plan
        .bundle
        .meta
        .diff_refs
        .start_sha
        .clone()
        .unwrap_or_default();

    // Concurrency guard
    let sem = Arc::new(Semaphore::new(pcfg.max_concurrency.max(1)));

    let mut futs = Vec::new();
    for d in drafts {
        // make everything owned for 'static future
        let http = http.clone();
        let headers = headers.clone();
        let base = base.to_string();
        let id = id.clone();
        let head = head.clone();
        let base_sha = base_sha.clone();
        let start_sha = start_sha.clone();
        let dry_run = pcfg.dry_run;
        let allow_edit = pcfg.allow_edit; // reserved for future "edit" support
        let existing = existing.clone();
        let sem_cloned = sem.clone();
        let draft = d.clone(); // <- FIX: avoid &draft in spawned task

        futs.push(tokio::spawn(async move {
            // use owned permit to satisfy 'static
            let _permit = sem_cloned.acquire_owned().await.unwrap();
            publish_one(
                &http, &headers, &base, &id, &draft, &head, &base_sha, &start_sha, dry_run,
                allow_edit, &existing,
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
async fn publish_one(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    id: &ChangeRequestId,
    draft: &DraftComment,
    head_sha: &str,
    base_sha: &str,
    start_sha: &str,
    dry_run: bool,
    _allow_edit: bool,
    existing: &HashSet<String>,
) -> MrResult<PublishedComment> {
    let (marker, key, _line_opt) = make_marker_and_key(draft);
    let body = format!("{}\n\n{}", render_title(draft), marker);

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
    let res = match &draft.target {
        TargetRef::Line { path, line } => {
            publish_inline(
                http, headers, base_api, id, path, *line, body, head_sha, base_sha, start_sha,
                dry_run,
            )
            .await
        }
        TargetRef::Range {
            path, start_line, ..
        } => {
            // GitLab inline position supports a single line. Use start_line.
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
                start_sha,
                dry_run,
            )
            .await
        }
        TargetRef::Symbol {
            path, decl_line, ..
        } => {
            publish_inline(
                http, headers, base_api, id, path, *decl_line, body, head_sha, base_sha, start_sha,
                dry_run,
            )
            .await
        }
        TargetRef::File { .. } | TargetRef::Global => {
            publish_general(http, headers, base_api, id, body, dry_run).await
        }
    }?;

    Ok(res)
}

/// Build a short human-friendly title for the comment (first line).
fn render_title(d: &DraftComment) -> String {
    // Take first non-empty line of the markdown body as a short title.
    let title = d
        .body_markdown
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Review note");
    title.to_string()
}

/// Construct inline discussion body and POST to GitLab.
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
    start_sha: &str,
    dry_run: bool,
) -> MrResult<PublishedComment> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/discussions",
        base_api, id.project, id.iid
    );

    // GitLab "text" position needs new_path + new_line + shas.
    #[derive(serde::Serialize)]
    struct Position<'a> {
        position_type: &'a str,
        new_path: &'a str,
        new_line: usize,
        head_sha: &'a str,
        base_sha: &'a str,
        start_sha: &'a str,
    }
    #[derive(serde::Serialize)]
    struct Req<'a> {
        body: &'a str,
        position: Position<'a>,
    }

    let req = Req {
        body: &body,
        position: Position {
            position_type: "text",
            new_path: path,
            new_line: line,
            head_sha,
            base_sha,
            start_sha,
        },
    };

    debug!(
        "step5: inline POST path={} line={} dry_run={}",
        path, line, dry_run
    );
    if dry_run {
        return Ok(PublishedComment {
            target: TargetRef::Line {
                path: path.to_string(),
                line,
            },
            performed: false,
            created_new: true,
            skipped_reason: Some("dry-run".into()),
            provider_ids: None,
        });
    }

    let resp = http
        .post(&url)
        .headers(headers.clone())
        .json(&req)
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(Error::Validation(format!(
            "gitlab inline post failed: status={} body={:?}",
            resp.status(),
            resp.text().await.ok()
        )));
    }

    // Extract minimal identifiers
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
            line,
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

/// General MR note (file/global).
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
        base_api, id.project, id.iid
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

    let resp = http
        .post(&url)
        .headers(headers.clone())
        .json(&Req { body: &body })
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(Error::Validation(format!(
            "gitlab note post failed: status={} body={:?}",
            resp.status(),
            resp.text().await.ok()
        )));
    }

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

/// Load existing discussion/note bodies and extract mrai markers for idempotency.
async fn load_existing_markers(
    http: &reqwest::Client,
    headers: &HeaderMap,
    base_api: &str,
    id: &ChangeRequestId,
) -> MrResult<HashSet<String>> {
    // We read only discussions; notes also come as discussions of type "Individual note".
    // If needed, you can also call /notes to be explicit.
    let url = format!(
        "{}/projects/{}/merge_requests/{}/discussions?per_page=100",
        base_api, id.project, id.iid
    );

    #[derive(serde::Deserialize)]
    struct Note {
        body: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Discussion {
        notes: Vec<Note>,
    }

    let resp = http.get(&url).headers(headers.clone()).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Validation(format!(
            "gitlab list discussions failed: status={} body={:?}",
            resp.status(),
            resp.text().await.ok()
        )));
    }

    let discussions: Vec<Discussion> = resp.json().await.unwrap_or_default();
    let mut set = HashSet::new();
    let re = Regex::new(r"<!--\s*mrai:key=([^;>]+);hash=([0-9a-f]+);ver=\d+\s*-->").unwrap();

    for d in discussions {
        for n in d.notes {
            if let Some(b) = n.body {
                if let Some(caps) = re.captures(&b) {
                    let key = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
                    let hash = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
                    set.insert(format!("{}#{}", key, hash));
                }
            }
        }
    }
    Ok(set)
}

/// A single place to build the *idempotency key* + marker string.
///
/// key format: "<path>:<line_or_decl_or_start>|<kind>"
/// - File/Global use "global" or "file:<path>"
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
    // embed also snippet_hash to key for ultra-idempotency
    let full_key = format!("{}#{}", key, d.snippet_hash);

    let marker = format!("<!-- mrai:key={};hash={};ver=1 -->", key, d.snippet_hash);

    (marker, full_key, line_opt)
}

fn build_http_client() -> MrResult<reqwest::Client> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .pool_idle_timeout(Some(Duration::from_secs(90)))
        .pool_max_idle_per_host(8)
        .build()?;
    Ok(client)
}

fn build_gitlab_headers(token: &str) -> MrResult<HeaderMap> {
    let mut h = HeaderMap::new();
    h.insert(USER_AGENT, HeaderValue::from_static("mr-reviewer/1.0"));
    h.insert(ACCEPT, HeaderValue::from_static("application/json"));
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    // GitLab Private Token header:
    h.insert(
        "PRIVATE-TOKEN",
        HeaderValue::from_str(token).map_err(|e| Error::Validation(format!("bad token: {e}")))?,
    );
    Ok(h)
}
