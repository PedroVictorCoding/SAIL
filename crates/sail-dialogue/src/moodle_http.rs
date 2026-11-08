//! SAIL Moodle owner API, nested at `/api/moodle`.
//!
//! Every route needs the owner bearer token. The token SAIL holds for Moodle never appears
//! in a reply; `/status` only says whether one is stored. Mirrored files are served by
//! content hash from the local mirror, never fetched from Moodle on request.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, NaiveDate, Utc};
use sail_guardian::authority::ApprovalAuthority;
use sail_moodle::service::{ConnectError, MoodleService};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Most changes one `/changes` reply carries.
pub const MAX_CHANGES_REPLY: usize = 200;

struct MoodleState {
    service: Arc<MoodleService>,
    authority: Option<ApprovalAuthority>,
}

type Failure = (StatusCode, Json<Value>);

pub fn router(service: Arc<MoodleService>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/launch", post(launch))
        .route("/connect", post(connect).delete(disconnect))
        .route("/sync", post(sync_now))
        .route("/pdf-permission", post(allow_pdfs).delete(stop_pdfs))
        .route("/courses", get(courses))
        .route("/changes", get(changes))
        .route("/changes/seen", post(mark_seen))
        .route("/items", get(items))
        .route("/items/:id/dismiss", post(dismiss))
        .route("/assessment", get(assessment))
        .route("/files", get(files))
        .route("/files/:sha256", get(file_bytes))
        .layer(axum::extract::DefaultBodyLimit::max(16_000))
        .with_state(Arc::new(MoodleState { service, authority }))
}

fn authorize(s: &MoodleState, h: &HeaderMap) -> Result<(), Failure> {
    let deny = |code| (code, Json(json!({"error":"Owner authentication required"})));
    let a = s
        .authority
        .as_ref()
        .ok_or_else(|| deny(StatusCode::SERVICE_UNAVAILABLE))?;
    let token = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| deny(StatusCode::UNAUTHORIZED))?;
    if !a.authenticates(token) {
        return Err(deny(StatusCode::UNAUTHORIZED));
    }
    Ok(())
}

fn bad(message: impl Into<String>) -> Failure {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
}

fn storage(e: impl std::fmt::Display) -> Failure {
    tracing::warn!(error = %e, "Moodle storage error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":"Moodle storage is unavailable."})),
    )
}

fn no_store(value: Value) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

async fn status(State(s): State<Arc<MoodleState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    Ok(no_store(json!(s.service.status())))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct LaunchBody {
    site_url: Option<String>,
}

async fn launch(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    body: Option<Json<LaunchBody>>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let (url, expires) = s
        .service
        .launch(body.site_url.as_deref())
        .map_err(|e| bad(e.to_string()))?;
    Ok(no_store(
        json!({"launch_url": url.as_str(), "expires_at": expires}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectBody {
    site_url: Option<String>,
    token: Option<String>,
    redirect: Option<String>,
}

async fn connect(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    body: Result<Json<ConnectBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let Json(body) = body.map_err(|e| bad(format!("Invalid request: {}", e.body_text())))?;
    let result = s
        .service
        .connect(
            body.site_url.as_deref(),
            body.token.as_deref(),
            body.redirect.as_deref(),
        )
        .await;
    match result {
        Ok(status) => Ok(no_store(json!(status))),
        Err(ConnectError::BadInput(m)) => Err(bad(m)),
        Err(ConnectError::Moodle(e)) => Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": e.to_string()})),
        )),
        Err(ConnectError::Storage(e)) => Err(storage(e)),
    }
}

async fn disconnect(State(s): State<Arc<MoodleState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    s.service.disconnect().map_err(storage)?;
    Ok(no_store(json!({"ok": true})))
}

/// The owner allows sandboxed PDF reading once (a Guardian standing rule for the fixed
/// `pdf-text-v1` operation; also listed and revocable under Guardian's rules). Refused when
/// the runner isn't installed, so the permission is never granted for nothing.
async fn allow_pdfs(State(s): State<Arc<MoodleState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    if s.service.pdf_reading() == sail_moodle::service::PdfReading::NotInstalled {
        return Err((
            StatusCode::CONFLICT,
            Json(
                json!({"error":"The PDF sandbox isn't installed. Run scripts/setup-sail-runner.sh, then restart SAIL."}),
            ),
        ));
    }
    let broker = sail_guardian::global_broker().map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"Guardian is unavailable."})),
        )
    })?;
    let rule = broker.policy().learn_pattern(
        crate::moodle_model::PDF_REQUESTER,
        sail_guardian::LearnedPattern::SandboxOperation {
            protocol: crate::moodle_model::PDF_PROTOCOL.into(),
        },
        "Allowed by the owner on the School page".into(),
    );
    tracing::info!(rule_id = %rule.id, "Owner allowed sandboxed PDF reading");
    // Read PDFs already mirrored now, locally; no Moodle requests.
    s.service.start_reprocess();
    Ok(no_store(json!(s.service.status())))
}

async fn stop_pdfs(State(s): State<Arc<MoodleState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    if let Ok(broker) = sail_guardian::global_broker() {
        broker.policy().revoke_sandbox_operation(
            crate::moodle_model::PDF_REQUESTER,
            crate::moodle_model::PDF_PROTOCOL,
        );
    }
    Ok(no_store(json!(s.service.status())))
}

async fn sync_now(State(s): State<Arc<MoodleState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    match s.service.request_sync() {
        Ok(()) => Ok(no_store(json!({"started": true}))),
        Err(message) => Err((StatusCode::CONFLICT, Json(json!({"error": message})))),
    }
}

async fn courses(State(s): State<Arc<MoodleState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let store = s.service.store();
    let today = chrono::Local::now().date_naive();
    let mut out = Vec::new();
    for course in store.courses().map_err(storage)? {
        let unseen = store
            .changes(None, Some(course.id), true, MAX_CHANGES_REPLY)
            .map_err(storage)?
            .len();
        let next = store
            .items(Some(course.id), Some(today), false)
            .map_err(storage)?
            .into_iter()
            .next();
        out.push(json!({
            "id": course.id,
            "short_name": course.short_name,
            "full_name": course.full_name,
            "start": course.start,
            "end": course.end,
            "unseen_changes": unseen,
            "next_item": next,
        }));
    }
    Ok(no_store(json!(out)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesQuery {
    since: Option<DateTime<Utc>>,
    course: Option<i64>,
    unseen: Option<bool>,
    limit: Option<usize>,
}

async fn changes(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    Query(q): Query<ChangesQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let limit = q.limit.unwrap_or(100).clamp(1, MAX_CHANGES_REPLY);
    let rows = s
        .service
        .store()
        .changes(q.since, q.course, q.unseen.unwrap_or(false), limit)
        .map_err(storage)?;
    Ok(no_store(json!(rows)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeenBody {
    ids: Vec<String>,
}

async fn mark_seen(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    body: Result<Json<SeenBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let Json(body) = body.map_err(|e| bad(format!("Invalid request: {}", e.body_text())))?;
    if body.ids.len() > MAX_CHANGES_REPLY {
        return Err(bad("Too many ids."));
    }
    let marked = s.service.store().mark_seen(&body.ids).map_err(storage)?;
    Ok(no_store(json!({"marked": marked})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemsQuery {
    course: Option<i64>,
    from: Option<NaiveDate>,
    dismissed: Option<bool>,
}

async fn items(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    Query(q): Query<ItemsQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let from = q.from.unwrap_or_else(|| chrono::Local::now().date_naive());
    let rows = s
        .service
        .store()
        .items(q.course, Some(from), q.dismissed.unwrap_or(false))
        .map_err(storage)?;
    Ok(no_store(json!(rows)))
}

async fn dismiss(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    if id.len() > 128 {
        return Err(bad("Unknown item."));
    }
    let ok = s.service.store().dismiss_item(&id).map_err(storage)?;
    Ok(no_store(json!({"ok": ok})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CourseQuery {
    course: i64,
}

async fn assessment(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    Query(q): Query<CourseQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let rows = s.service.store().assessment(q.course).map_err(storage)?;
    Ok(no_store(json!(rows)))
}

async fn files(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    Query(q): Query<CourseQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let store = s.service.store();
    let versions = store.file_versions(q.course).map_err(storage)?;
    let mut out: Vec<Value> = versions
        .iter()
        .map(|v| {
            json!({
                "key": v.key,
                "size": v.size,
                "modified": v.modified,
                "sha256": v.sha256,
                "downloaded_at": v.downloaded_at,
                "too_large": false,
            })
        })
        .collect();
    if let Some(snapshot) = store.snapshot(q.course).map_err(storage)? {
        for file in sail_moodle::service::snapshot_files(&snapshot) {
            if file.size > sail_moodle::MAX_FILE_BYTES {
                out.push(json!({
                    "key": file.key(),
                    "size": file.size,
                    "modified": file.modified,
                    "sha256": null,
                    "downloaded_at": null,
                    "too_large": true,
                }));
            }
        }
    }
    Ok(no_store(json!(out)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileQuery {
    name: Option<String>,
}

async fn file_bytes(
    State(s): State<Arc<MoodleState>>,
    h: HeaderMap,
    Path(sha256): Path<String>,
    Query(q): Query<FileQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"No such file."})),
        )
    };
    let path = s.service.store().blob_path(&sha256).ok_or_else(not_found)?;
    let bytes = tokio::fs::read(&path).await.map_err(|_| not_found())?;
    let name = q
        .name
        .as_deref()
        .map(safe_download_name)
        .unwrap_or_else(|| format!("{}.bin", &sha256[..12.min(sha256.len())]));
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// A download name from an untrusted Moodle file name: printable ASCII, no quotes, slashes
/// or backslashes, at most 120 chars. Non-ASCII letters become `_` (the header is ASCII).
fn safe_download_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if c.is_ascii_graphic() && !matches!(c, '"' | '/' | '\\' | ';') || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        "file".into()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_names_are_header_safe() {
        assert_eq!(
            safe_download_name("Enunciado \"P1\".pdf"),
            "Enunciado _P1_.pdf"
        );
        assert_eq!(safe_download_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(
            safe_download_name("Ficha da UC – 2026.pdf"),
            "Ficha da UC _ 2026.pdf"
        );
        assert_eq!(safe_download_name("\r\n"), "file");
    }
}
