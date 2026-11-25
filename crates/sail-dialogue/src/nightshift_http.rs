//! Owner-only Night Shift endpoints (round 27), nested at `/api/nightshift`.
//!
//! Shapes are the round-27 HTTP contract in `LLM_Handoff.md`; the web and Android clients and
//! their fixtures (`scripts/ui/fixtures/nightshift/`) are built against them. Every route needs
//! the owner bearer. Errand changes and the morning report also reach open voice sockets as
//! `{"type":"nightshift",…}` / `{"type":"nightshift_morning",…}` (see `server.rs`).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use chrono::Local;
use sail_forge::nightshift::plan::{PlanErrand, SizeDurations, plan_night};
use sail_forge::nightshift::{ErrandId, ErrandSource, ErrandState, NightShiftSettings, WorkOrder};
use sail_guardian::authority::ApprovalAuthority;
use sail_protocol::mesh::MeshWorkerState;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::nightshift::{ApproveError, NightShift};

/// Largest request body, in bytes. Fixed a priori: the biggest body is an edited work order
/// (≤ ~4.5 KB of text by its own limits) or a 2 000-char ask; 32 KB leaves room for JSON
/// escaping and rejects anything hostile.
const MAX_BODY_BYTES: usize = 32 * 1024;

struct HttpState {
    service: Arc<NightShift>,
    mind: Option<sail_mind::MindService>,
    authority: Option<ApprovalAuthority>,
}

/// Night Shift routes. Nest at `/api/nightshift`; the index route answers `GET /api/nightshift`.
pub fn router(
    service: Arc<NightShift>,
    mind: Option<sail_mind::MindService>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    Router::new()
        .route("/", get(overview))
        .route("/errands", post(add))
        .route("/errands/:id/approve", post(approve))
        .route("/errands/:id/reshape", post(reshape))
        .route("/errands/:id/redo", post(redo))
        .route("/errands/:id/ship", post(ship))
        .route("/errands/:id/drop", post(drop_errand))
        .route("/errands/:id/diff", get(diff))
        .route("/settings", put(update_settings))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(HttpState {
            service,
            mind,
            authority,
        }))
}

type Failure = (StatusCode, Json<Value>);
type Reply = Result<(StatusCode, Json<Value>), Failure>;

fn fail(status: StatusCode, message: impl Into<String>) -> Failure {
    (status, Json(json!({"error": message.into()})))
}

fn authorize(state: &HttpState, headers: &HeaderMap) -> Result<(), Failure> {
    let authority = state
        .authority
        .as_ref()
        .ok_or_else(|| fail(StatusCode::SERVICE_UNAVAILABLE, "owner token required"))?;
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "owner token required"))?;
    if !authority.authenticates(token) {
        return Err(fail(StatusCode::UNAUTHORIZED, "owner token required"));
    }
    Ok(())
}

fn errand_id(raw: &str) -> Result<ErrandId, Failure> {
    ErrandId::parse(raw).ok_or_else(|| fail(StatusCode::NOT_FOUND, "no such errand"))
}

/// Store refusals are plain-words strings; map the ones the contract names.
fn refused(message: String) -> Failure {
    if message == "no such errand" {
        fail(StatusCode::NOT_FOUND, message)
    } else if message.starts_with("can't ") {
        fail(StatusCode::CONFLICT, message)
    } else {
        fail(StatusCode::UNPROCESSABLE_ENTITY, message)
    }
}

fn window_label(settings: &NightShiftSettings) -> String {
    format!(
        "{:02}:00–{:02}:00",
        settings.window_start_hour % 24,
        settings.window_end_hour % 24
    )
}

fn window_secs(settings: &NightShiftSettings) -> u64 {
    let start = u64::from(settings.window_start_hour % 24);
    let end = u64::from(settings.window_end_hour % 24);
    let hours = if end > start {
        end - start
    } else {
        24 - start + end
    };
    hours * 3_600
}

fn plan_preview(service: &NightShift, settings: &NightShiftSettings) -> Value {
    let errands: Vec<PlanErrand> = service
        .store()
        .list()
        .into_iter()
        .filter_map(|errand| match errand.state {
            ErrandState::Queued { order, approved_at } => Some(PlanErrand {
                has_redo_note: errand.attempts > 0
                    && errand
                        .notes
                        .last()
                        .is_some_and(|note| note.at >= approved_at),
                id: errand.id,
                size: order.size,
                approved_at,
                attempts: errand.attempts,
            }),
            _ => None,
        })
        .collect();
    let plan = plan_night(
        &errands,
        window_secs(settings),
        settings.max_items.clamp(1, 10),
        &SizeDurations::default(),
    );
    json!(plan)
}

fn mesh_summary(state: &HttpState) -> Value {
    let Some(pool) = state.mind.as_ref().and_then(|mind| mind.mesh()) else {
        return Value::Null;
    };
    let workers = pool.status().workers;
    let chosen = workers
        .iter()
        .find(|worker| worker.state == MeshWorkerState::Ready)
        .or_else(|| {
            workers
                .iter()
                .find(|worker| worker.state == MeshWorkerState::Busy)
        })
        .or_else(|| workers.first());
    match chosen {
        Some(worker) => json!({
            "available": worker.state == MeshWorkerState::Ready,
            "name": worker.name,
            "resting": worker.state == MeshWorkerState::Busy,
        }),
        None => Value::Null,
    }
}

fn external_summary(settings: &NightShiftSettings) -> Value {
    let state = sail_forge::nightly::NightlyState::load(&sail_forge::nightly::default_state_dir());
    let today = Local::now().format("%Y-%m-%d").to_string();
    json!({
        "provider": settings.external_provider,
        "used_tonight": state.external_used.get(&today).copied().unwrap_or(0),
        "allowed": settings.external_solves_per_night,
    })
}

async fn overview(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    let store = state.service.store();
    let settings = store.settings();
    let today = Local::now().format("%Y-%m-%d").to_string();
    Ok((
        StatusCode::OK,
        Json(json!({
            "settings": settings,
            "tonight": {
                "window": window_label(&settings),
                "plan": plan_preview(&state.service, &settings),
            },
            "errands": store.list(),
            "morning": store.morning(&today),
            "mesh": mesh_summary(&state),
            "external": external_summary(&settings),
        })),
    ))
}

#[derive(Deserialize)]
struct AddBody {
    ask: String,
    #[serde(default = "default_source")]
    source: ErrandSource,
}

fn default_source() -> ErrandSource {
    ErrandSource::Web
}

async fn add(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<AddBody>,
) -> Reply {
    authorize(&state, &headers)?;
    let errand = state
        .service
        .add(&body.ask, body.source)
        .map_err(|error| fail(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok((StatusCode::CREATED, Json(json!(errand))))
}

#[derive(Deserialize, Default)]
struct ApproveBody {
    #[serde(default)]
    order: Option<WorkOrder>,
}

async fn approve(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<ApproveBody>>,
) -> Reply {
    authorize(&state, &headers)?;
    let id = errand_id(&id)?;
    let order = body.and_then(|Json(body)| body.order);
    match state.service.approve(&id, order) {
        Ok(errand) => Ok((StatusCode::OK, Json(json!(errand)))),
        Err(ApproveError::Invalid(reasons)) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"reasons": reasons})),
        )),
        Err(ApproveError::Refused(message)) => Err(refused(message)),
    }
}

#[derive(Deserialize)]
struct NoteBody {
    note: String,
}

async fn reshape(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NoteBody>,
) -> Reply {
    authorize(&state, &headers)?;
    let id = errand_id(&id)?;
    let errand = state.service.reshape(&id, &body.note).map_err(refused)?;
    Ok((StatusCode::ACCEPTED, Json(json!(errand))))
}

async fn redo(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NoteBody>,
) -> Reply {
    authorize(&state, &headers)?;
    let id = errand_id(&id)?;
    let errand = state.service.redo(&id, &body.note).map_err(refused)?;
    Ok((StatusCode::OK, Json(json!(errand))))
}

async fn ship(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&state, &headers)?;
    let id = errand_id(&id)?;
    let errand = state.service.ship(&id).map_err(|message| {
        if message.starts_with("there is no stored patch") {
            fail(StatusCode::CONFLICT, message)
        } else {
            refused(message)
        }
    })?;
    Ok((StatusCode::ACCEPTED, Json(json!(errand))))
}

async fn drop_errand(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&state, &headers)?;
    let id = errand_id(&id)?;
    let errand = state.service.drop_errand(&id).map_err(refused)?;
    Ok((StatusCode::OK, Json(json!(errand))))
}

async fn diff(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    authorize(&state, &headers)?;
    let id = errand_id(&id)?;
    let patch = state
        .service
        .diff(&id)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "no stored diff for this errand"))?;
    Ok((
        [(header::CONTENT_TYPE, "text/x-diff; charset=utf-8")],
        patch,
    )
        .into_response())
}

async fn update_settings(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(settings): Json<NightShiftSettings>,
) -> Reply {
    authorize(&state, &headers)?;
    let saved = state
        .service
        .store()
        .set_settings(settings)
        .map_err(|reasons| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": reasons.join("; "), "reasons": reasons})),
            )
        })?;
    // The bare settings object: both clients read the reply as `NightShiftSettings`.
    Ok((StatusCode::OK, Json(json!(saved))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_label_and_length_wrap_past_midnight() {
        let settings = NightShiftSettings::default();
        assert_eq!(window_label(&settings), "01:00–07:00");
        assert_eq!(window_secs(&settings), 6 * 3_600);
        let late = NightShiftSettings {
            window_start_hour: 23,
            window_end_hour: 5,
            ..NightShiftSettings::default()
        };
        assert_eq!(window_label(&late), "23:00–05:00");
        assert_eq!(window_secs(&late), 6 * 3_600);
    }

    #[test]
    fn store_refusals_map_to_contract_statuses() {
        assert_eq!(refused("no such errand".into()).0, StatusCode::NOT_FOUND);
        assert_eq!(
            refused("can't approve while queued".into()).0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            refused("the note must be 1..=1000 characters".into()).0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
