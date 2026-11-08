//! Course Factory owner API, merged into `/api/learn` under `/factory`.
//!
//! Every route needs the owner token. No route calls a model: imports parse with the pure
//! extractors (off the async runtime), and everything else reads stores or queues jobs. The
//! contract is written down in the factory handoff and mirrored by `web/learn-factory.js`.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use sail_arena_learn::factory::ingest::MAX_FILE_BYTES;
use sail_arena_learn::factory::service::{
    FactoryError, FactoryService, ImportRequest, parse_rights,
};
use sail_arena_learn::factory::service::{NewCourse, OwnerGraphEdit};
use sail_arena_learn::factory::settings::FactorySettings;
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct FactoryState {
    factory: Arc<FactoryService>,
    authority: Option<ApprovalAuthority>,
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

/// Small JSON bodies for everything except the upload route.
const JSON_BODY_LIMIT: usize = 64 * 1024;

pub fn router(factory: Arc<FactoryService>, authority: Option<ApprovalAuthority>) -> Router {
    let state = Arc::new(FactoryState { factory, authority });
    let upload = Router::new()
        .route("/factory/sources", post(import))
        // The cap the extractors enforce, plus room for nothing else: the body is the file.
        .layer(axum::extract::DefaultBodyLimit::max(MAX_FILE_BYTES))
        .with_state(state.clone());
    let rest = Router::new()
        .route("/factory/sources", get(library))
        .route("/factory/sources/:id", delete(remove))
        .route("/factory/jobs", get(jobs))
        .route("/factory/jobs/pause", post(pause))
        .route("/factory/jobs/resume", post(resume))
        .route("/factory/settings", get(settings).put(save_settings))
        .route("/factory/courses", get(courses).post(create_course))
        .route("/factory/courses/:id", get(course))
        .route("/factory/courses/:id/plan/approve", post(approve_plan))
        .route("/factory/courses/:id/graph/edits", post(edit_graph))
        .route("/factory/courses/:id/pause", post(pause_course))
        .route("/factory/courses/:id/resume", post(resume_course))
        .route("/factory/courses/:id/next", post(next_lesson))
        .route("/factory/drafts/:id", get(draft))
        .route("/factory/drafts/:id/publish", post(publish_draft))
        .route("/factory/drafts/:id/reject", post(reject_draft))
        .layer(axum::extract::DefaultBodyLimit::max(JSON_BODY_LIMIT))
        .with_state(state);
    upload.merge(rest)
}

fn authorize(s: &FactoryState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let deny = |code| {
        (
            code,
            Json(json!({"error": "Owner authentication required"})),
        )
    };
    let authority = s
        .authority
        .as_ref()
        .ok_or_else(|| deny(StatusCode::SERVICE_UNAVAILABLE))?;
    let token = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| deny(StatusCode::UNAUTHORIZED))?;
    if !authority.authenticates(token) {
        return Err(deny(StatusCode::UNAUTHORIZED));
    }
    Ok(())
}

fn fail(e: FactoryError) -> (StatusCode, Json<Value>) {
    let code = match &e {
        FactoryError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        FactoryError::NotFound(_) => StatusCode::NOT_FOUND,
        FactoryError::Conflict(_) => StatusCode::CONFLICT,
        FactoryError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if code == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::warn!(error = %e, "Course Factory storage error");
        return (
            code,
            Json(json!({"error": "Course Factory storage is unavailable."})),
        );
    }
    (code, Json(json!({"error": e.to_string()})))
}

fn ok(value: impl serde::Serialize) -> Reply {
    serde_json::to_value(value).map(Json).map_err(|e| {
        tracing::warn!(error = %e, "Course Factory reply didn't serialize");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Course Factory reply failed."})),
        )
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportQuery {
    file_name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    rights: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

async fn import(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> Reply {
    authorize(&s, &h)?;
    if body.is_empty() {
        return Err(fail(FactoryError::Invalid("the file is empty".into())));
    }
    let request = ImportRequest {
        file_name: q.file_name,
        title: q.title.filter(|t| !t.is_empty()),
        rights: parse_rights(
            q.rights.as_deref().filter(|r| !r.is_empty()),
            q.license.as_deref(),
        )
        .map_err(fail)?,
        // The web form sends empty fields for blanks: treat them as "not given".
        scope: q
            .scope
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "learning:general".into()),
    };
    let factory = s.factory.clone();
    let result = tokio::task::spawn_blocking(move || factory.import(&request, &body))
        .await
        .map_err(|_| {
            fail(FactoryError::Store(
                "the import stopped unexpectedly".into(),
            ))
        })?;
    let (source, duplicate) = result.map_err(fail)?;
    ok(json!({"source": source, "duplicate": duplicate}))
}

async fn library(State(s): State<Arc<FactoryState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let factory = s.factory.clone();
    let sources = tokio::task::spawn_blocking(move || factory.library())
        .await
        .map_err(|_| fail(FactoryError::Store("the library read stopped".into())))?
        .map_err(fail)?;
    ok(json!({"sources": sources}))
}

async fn remove(State(s): State<Arc<FactoryState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    let invalidation = s.factory.remove_source(&id).map_err(fail)?;
    ok(json!({"invalidation": invalidation}))
}

async fn jobs(State(s): State<Arc<FactoryState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    ok(s.factory.jobs_view().map_err(fail)?)
}

async fn pause(State(s): State<Arc<FactoryState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    ok(json!({"paused": s.factory.set_paused(true).map_err(fail)?}))
}

async fn resume(State(s): State<Arc<FactoryState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    ok(json!({"paused": s.factory.set_paused(false).map_err(fail)?}))
}

async fn settings(State(s): State<Arc<FactoryState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    ok(json!({"settings": s.factory.settings().map_err(fail)?}))
}

async fn save_settings(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Json(body): Json<FactorySettings>,
) -> Reply {
    authorize(&s, &h)?;
    ok(json!({"settings": s.factory.save_settings(&body).map_err(fail)?}))
}

/// Run a blocking service call off the async runtime. Course operations read and write
/// SQLite and may assemble a package, so they never run on a runtime thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, FactoryError> + Send + 'static,
) -> Result<T, (StatusCode, Json<Value>)> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| {
            fail(FactoryError::Store(
                "the request stopped unexpectedly".into(),
            ))
        })?
        .map_err(fail)
}

async fn courses(State(s): State<Arc<FactoryState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(json!({"courses": blocking(move || f.courses()).await?}))
}

async fn create_course(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Json(body): Json<NewCourse>,
) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(json!({"course": blocking(move || f.create_course(&body)).await?}))
}

async fn course(State(s): State<Arc<FactoryState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(blocking(move || f.course(&id)).await?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionBody {
    revision: u64,
}

async fn approve_plan(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RevisionBody>,
) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(json!({"plan": blocking(move || f.approve_plan(&id, body.revision)).await?}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditsBody {
    base_revision: u64,
    edits: Vec<OwnerGraphEdit>,
}

async fn edit_graph(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<EditsBody>,
) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    let graph = blocking(move || f.edit_graph(&id, body.base_revision, body.edits)).await?;
    ok(json!({"graph": graph}))
}

async fn pause_course(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(json!({"course": blocking(move || f.set_course_paused(&id, true)).await?}))
}

async fn resume_course(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(json!({"course": blocking(move || f.set_course_paused(&id, false)).await?}))
}

async fn next_lesson(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(json!({"queued": blocking(move || f.next_lesson(&id)).await?}))
}

async fn draft(State(s): State<Arc<FactoryState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(blocking(move || f.draft(&id)).await?)
}

async fn publish_draft(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    let (course_id, revision) = blocking(move || f.publish_draft(&id)).await?;
    ok(json!({"course_id": course_id, "revision": revision}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectBody {
    #[serde(default)]
    reason: String,
}

async fn reject_draft(
    State(s): State<Arc<FactoryState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RejectBody>,
) -> Reply {
    authorize(&s, &h)?;
    let f = s.factory.clone();
    ok(json!({"draft": blocking(move || f.reject_draft(&id, &body.reason)).await?}))
}
