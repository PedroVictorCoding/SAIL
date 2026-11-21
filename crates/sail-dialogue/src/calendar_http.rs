//! Owner-only SAIL Calendar endpoints (round 28), nested at `/api/calendar`.
//!
//! Shapes are the round-28 HTTP contract in `LLM_Handoff.md`; the web and Android clients
//! and their fixtures (`scripts/ui/fixtures/calendar/`) are built against them. Every
//! route needs the owner bearer. Errors are `{"error":{"code":…,"message":…}}`. Store
//! changes also reach open voice sockets as `{"type":"calendar_changed",…}` (`server.rs`).

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use chrono::{DateTime, Utc};
use sail_calendar::contracts::{
    CalendarError, DeleteScope, EventDraft, EventPatch, NewFeed, Preferences, SourcePatch,
};
use sail_calendar::service::CalendarService;
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};

/// Largest JSON body. Fixed a priori: the biggest is an event with 4 000-char notes.
const MAX_JSON_BYTES: usize = 32 * 1024;
/// Largest `.ics` import, the same 5 MiB cap as a feed (invariant 4).
const MAX_IMPORT_BYTES: usize = 5 * 1024 * 1024;

struct HttpState {
    service: Arc<CalendarService>,
    authority: Option<ApprovalAuthority>,
}

/// Calendar routes. Nest at `/api/calendar`.
pub fn router(service: Arc<CalendarService>, authority: Option<ApprovalAuthority>) -> Router {
    let state = Arc::new(HttpState { service, authority });
    let import = Router::new()
        .route("/import/preview", post(import_preview))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_IMPORT_BYTES));
    Router::new()
        .route("/sources", get(sources).post(add_feed))
        .route("/sources/:id", patch(update_source).delete(remove_source))
        .route("/sources/:id/sync", post(sync_source))
        .route("/events", get(events).post(create_event))
        .route(
            "/events/:id",
            get(event).patch(update_event).delete(delete_event),
        )
        .route("/import/commit", post(import_commit))
        .route("/export.ics", get(export))
        .route("/freebusy", get(freebusy))
        .route("/preferences", get(preferences).put(set_preferences))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_JSON_BYTES))
        .merge(import)
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(state)
}

type Failure = (StatusCode, Json<Value>);

fn fail(status: StatusCode, code: &str, message: impl Into<String>) -> Failure {
    (
        status,
        Json(json!({"error": {"code": code, "message": message.into()}})),
    )
}

fn refused(error: CalendarError) -> Failure {
    let status = match &error {
        CalendarError::Invalid(_) => StatusCode::BAD_REQUEST,
        CalendarError::NotFound => StatusCode::NOT_FOUND,
        CalendarError::ReadOnly => StatusCode::FORBIDDEN,
        CalendarError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        CalendarError::TooManySources => StatusCode::CONFLICT,
        CalendarError::BlockedAddress
        | CalendarError::NotCalendar
        | CalendarError::TooLarge
        | CalendarError::FetchFailed(_) => StatusCode::UNPROCESSABLE_ENTITY,
        CalendarError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let message = match &error {
        // Never echo storage internals to the client.
        CalendarError::Storage(_) => "the calendar store failed".to_string(),
        CalendarError::Invalid(reason) => reason.clone(),
        other => other.to_string(),
    };
    fail(status, error.code(), message)
}

fn authorize(state: &HttpState, headers: &HeaderMap) -> Result<(), Failure> {
    let unauthorized = || {
        fail(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "owner token required",
        )
    };
    let authority = state.authority.as_ref().ok_or_else(|| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "unauthorized",
            "owner token required",
        )
    })?;
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(unauthorized)?;
    if !authority.authenticates(token) {
        return Err(unauthorized());
    }
    Ok(())
}

fn ok<T: serde::Serialize>(status: StatusCode, value: T) -> Response {
    (status, Json(value)).into_response()
}

fn reply<T: serde::Serialize>(result: Result<T, CalendarError>) -> Response {
    match result {
        Ok(value) => ok(StatusCode::OK, value),
        Err(error) => refused(error).into_response(),
    }
}

fn split_ids(raw: Option<&str>) -> Option<Vec<String>> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn instant(raw: &str, field: &str) -> Result<DateTime<Utc>, Failure> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| {
            fail(
                StatusCode::BAD_REQUEST,
                "invalid",
                format!("{field} must be an RFC 3339 time"),
            )
        })
}

async fn sources(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    match state.service.sources() {
        Ok(sources) => ok(StatusCode::OK, json!({ "sources": sources })),
        Err(error) => refused(error).into_response(),
    }
}

async fn add_feed(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(feed): Json<NewFeed>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    match state.service.add_feed(feed, Utc::now()).await {
        Ok(source) => ok(StatusCode::CREATED, source),
        Err(error) => refused(error).into_response(),
    }
}

async fn update_source(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<SourcePatch>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    reply(state.service.update_source(&id, patch))
}

async fn remove_source(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    match state.service.remove_source(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => refused(error).into_response(),
    }
}

async fn sync_source(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    reply(state.service.sync_source(&id, Utc::now()).await)
}

#[derive(Deserialize)]
struct WindowQuery {
    start: String,
    end: String,
    sources: Option<String>,
    #[serde(default)]
    include_cancelled: bool,
}

async fn events(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(query): Query<WindowQuery>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    let (start, end) = match (instant(&query.start, "start"), instant(&query.end, "end")) {
        (Ok(start), Ok(end)) => (start, end),
        (Err(failure), _) | (_, Err(failure)) => return failure.into_response(),
    };
    let ids = split_ids(query.sources.as_deref());
    reply(
        state
            .service
            .window(start, end, ids.as_deref(), query.include_cancelled),
    )
}

async fn event(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    reply(state.service.event(&id))
}

async fn create_event(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(draft): Json<EventDraft>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    match state.service.create(draft, Utc::now()) {
        Ok(event) => ok(StatusCode::CREATED, event),
        Err(error) => refused(error).into_response(),
    }
}

async fn update_event(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<EventPatch>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    reply(state.service.update(&id, patch, Utc::now()))
}

#[derive(Deserialize)]
struct DeleteQuery {
    scope: Option<DeleteScope>,
    occurrence: Option<String>,
}

async fn delete_event(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    let scope = query.scope.unwrap_or(DeleteScope::All);
    match state
        .service
        .delete(&id, scope, query.occurrence.as_deref())
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => refused(error).into_response(),
    }
}

async fn import_preview(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    let text = String::from_utf8_lossy(&body);
    if !text.trim_start().starts_with("BEGIN:VCALENDAR") {
        return refused(CalendarError::NotCalendar).into_response();
    }
    reply(state.service.import_preview(&text, Utc::now()))
}

#[derive(Deserialize)]
struct CommitBody {
    preview_id: String,
}

async fn import_commit(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<CommitBody>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    reply(state.service.import_commit(&body.preview_id, Utc::now()))
}

#[derive(Deserialize)]
struct ExportQuery {
    sources: Option<String>,
    start: Option<String>,
    end: Option<String>,
}

async fn export(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(query): Query<ExportQuery>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    let parse = |raw: Option<&String>, field: &str| -> Result<Option<DateTime<Utc>>, Failure> {
        raw.map(|value| instant(value, field)).transpose()
    };
    let (start, end) = match (
        parse(query.start.as_ref(), "start"),
        parse(query.end.as_ref(), "end"),
    ) {
        (Ok(start), Ok(end)) => (start, end),
        (Err(failure), _) | (_, Err(failure)) => return failure.into_response(),
    };
    let ids = split_ids(query.sources.as_deref());
    match state.service.export(ids.as_deref(), start, end) {
        Ok(text) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/calendar; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"sail-calendar.ics\"",
                ),
            ],
            text,
        )
            .into_response(),
        Err(error) => refused(error).into_response(),
    }
}

#[derive(Deserialize)]
struct BusyQuery {
    start: String,
    end: String,
}

async fn freebusy(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(query): Query<BusyQuery>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    let (start, end) = match (instant(&query.start, "start"), instant(&query.end, "end")) {
        (Ok(start), Ok(end)) => (start, end),
        (Err(failure), _) | (_, Err(failure)) => return failure.into_response(),
    };
    match state.service.freebusy(start, end) {
        Ok(busy) => ok(StatusCode::OK, json!({ "busy": busy })),
        Err(error) => refused(error).into_response(),
    }
}

async fn preferences(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    reply(state.service.preferences())
}

async fn set_preferences(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(prefs): Json<Preferences>,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return failure.into_response();
    }
    reply(state.service.set_preferences(prefs))
}
