//! Owner-only scheduler API, `/api/agents/schedule` (round 21). The Schedule section of
//! SAIL Agents uses it; see [`crate::schedule_runner`] for how jobs run.
use crate::schedule_runner::ScheduleService;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::Utc;
use sail_flow::schedule::{Action, Creator, Deliver};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct HttpState {
    service: Arc<ScheduleService>,
    authority: Option<ApprovalAuthority>,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message.into()})))
}

fn authorize(s: &HttpState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let authority = s.authority.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner authority unavailable",
        )
    })?;
    let token = h
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if token.is_some_and(|t| authority.authenticates(t)) {
        Ok(())
    } else {
        Err(error(StatusCode::UNAUTHORIZED, "owner token required"))
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn router(service: Arc<ScheduleService>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/", get(list).post(create))
        .route("/preview", post(preview))
        .route("/:id", axum::routing::delete(delete))
        .route("/:id/runs", get(runs))
        .route("/:id/:action", post(act))
        .layer(axum::extract::DefaultBodyLimit::max(32_000))
        .with_state(Arc::new(HttpState { service, authority }))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
    status: StatusCode,
) -> Result<T, (StatusCode, Json<Value>)> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "scheduler failed"))?
        .map_err(|e| error(status, e))
}

async fn list(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let service = s.service.clone();
    let now = Utc::now();
    let body = blocking(
        move || {
            let jobs = service.store.jobs()?;
            let ts = now.timestamp();
            let mut views: Vec<Value> = jobs.iter().map(|j| service.view(j, ts)).collect();
            // Proposed first, then by next run.
            views.sort_by_key(|v| {
                (
                    v["state"] != "proposed",
                    v["next_run"].as_str().map(String::from).unwrap_or_default(),
                )
            });
            Ok(json!({"status": service.status(now), "jobs": views}))
        },
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await?;
    Ok(Json(body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewBody {
    when: String,
    tz: String,
}

async fn preview(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Json(b): Json<PreviewBody>,
) -> Reply {
    authorize(&s, &h)?;
    ScheduleService::preview(&b.when, &b.tz, Utc::now().timestamp())
        .map(Json)
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    name: String,
    when: String,
    tz: String,
    action: Action,
    deliver: Deliver,
}

async fn create(State(s): State<Arc<HttpState>>, h: HeaderMap, Json(b): Json<CreateBody>) -> Reply {
    authorize(&s, &h)?;
    let service = s.service.clone();
    let view = blocking(
        move || {
            let now = Utc::now().timestamp();
            let job = service.create(
                &b.name,
                &b.when,
                &b.tz,
                b.action,
                b.deliver,
                Creator::Owner,
                None,
                now,
            )?;
            Ok(service.view(&job, now))
        },
        StatusCode::BAD_REQUEST,
    )
    .await?;
    tracing::info!(job = %view["id"], "Scheduled job created by owner");
    Ok(Json(json!({"job": view})))
}

async fn act(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path((id, action)): Path<(String, String)>,
) -> Reply {
    authorize(&s, &h)?;
    if !valid_id(&id) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid job id"));
    }
    let service = s.service.clone();
    let now = Utc::now().timestamp();
    let job = match action.as_str() {
        "run" => {
            // Starting a run spawns its task, so it stays on the async side.
            service
                .run_now(&id, now)
                .map_err(|e| error(StatusCode::CONFLICT, e))?
        }
        "pause" | "resume" | "approve" => {
            let worker = service.clone();
            blocking(move || worker.set_state(&id, &action), StatusCode::CONFLICT).await?
        }
        "reject" => {
            let worker = service.clone();
            blocking(
                move || {
                    let job = worker.store.get(&id)?;
                    if job.state != sail_flow::schedule::JobState::Proposed {
                        return Err("only proposed jobs can be rejected".to_string());
                    }
                    worker.store.delete(&id)
                },
                StatusCode::CONFLICT,
            )
            .await?;
            return Ok(Json(json!({"ok": true})));
        }
        _ => return Err(error(StatusCode::NOT_FOUND, "unknown action")),
    };
    tracing::info!(job = %job.id, state = ?job.state, "Scheduled job changed by owner");
    Ok(Json(json!({"job": service.view(&job, now)})))
}

async fn delete(State(s): State<Arc<HttpState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    if !valid_id(&id) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid job id"));
    }
    let service = s.service.clone();
    blocking(move || service.store.delete(&id), StatusCode::NOT_FOUND).await?;
    Ok(Json(json!({"ok": true})))
}

async fn runs(State(s): State<Arc<HttpState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    if !valid_id(&id) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid job id"));
    }
    let service = s.service.clone();
    let runs = blocking(
        move || {
            service.store.get(&id)?;
            service.store.runs(&id)
        },
        StatusCode::NOT_FOUND,
    )
    .await?;
    Ok(Json(json!({"runs": runs})))
}
