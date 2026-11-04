//! Owner-only Pulse controls, nested at `/api/pulse`.
//!
//! The owner token, not a model tool, authorizes changing the user's reminders through the
//! interface. The same store is used by the scheduler and the conversational tools.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::{DateTime, Duration, Utc};
use sail_guardian::authority::ApprovalAuthority;
use sail_pulse::{NewReminder, Pulse, PulseError, PulseSettings};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct HttpState {
    pulse: Arc<Pulse>,
    authority: Option<ApprovalAuthority>,
}

/// Owner-only reminder controls.
pub fn router(pulse: Arc<Pulse>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/reminders", get(list).post(create))
        .route("/reminders/:id/acknowledge", post(acknowledge))
        .route("/reminders/:id/cancel", post(cancel))
        .route("/reminders/:id/snooze", post(snooze))
        .route("/settings", get(settings).post(update_settings))
        .layer(axum::extract::DefaultBodyLimit::max(64_000))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(HttpState { pulse, authority }))
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn refuse(status: StatusCode) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error":"owner token required"})))
}

fn authorize(state: &HttpState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let authority = state
        .authority
        .as_ref()
        .ok_or_else(|| refuse(StatusCode::SERVICE_UNAVAILABLE))?;
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| refuse(StatusCode::UNAUTHORIZED))?;
    if !authority.authenticates(token) {
        return Err(refuse(StatusCode::UNAUTHORIZED));
    }
    Ok(())
}

fn failure(error: PulseError) -> (StatusCode, Json<Value>) {
    let status = match &error {
        PulseError::NotFound(_) => StatusCode::NOT_FOUND,
        PulseError::Invalid(_) => StatusCode::BAD_REQUEST,
        PulseError::InvalidState { .. } => StatusCode::CONFLICT,
        PulseError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({"error":error.to_string()})))
}

fn invalid(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":message.into()})),
    )
}

async fn list(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    // The scheduler runs every second; this also makes a direct HTTP read correct when the
    // store is used without the daemon's scheduler (for example in a focused test).
    state.pulse.fire_due().map_err(failure)?;
    Ok(Json(json!({
        "reminders": state.pulse.list_active(sail_pulse::DEFAULT_LIST_LIMIT).map_err(failure)?,
        "history": state.pulse.list_history(sail_pulse::DEFAULT_HISTORY_LIMIT).map_err(failure)?,
        "settings": state.pulse.settings().map_err(failure)?,
        "now": Utc::now(),
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    text: String,
    due_at: String,
    timezone: String,
    utc_offset_minutes: i32,
}

fn parse_due(value: &str) -> Result<DateTime<Utc>, (StatusCode, Json<Value>)> {
    DateTime::parse_from_rfc3339(value)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| invalid("due_at must be an RFC 3339 timestamp with an offset"))
}

async fn create(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(request): Json<CreateRequest>,
) -> Reply {
    authorize(&state, &headers)?;
    let due_at = parse_due(&request.due_at)?;
    let new = NewReminder::new(
        request.text,
        due_at,
        request.timezone,
        request.utc_offset_minutes,
    )
    .map_err(failure)?;
    let reminder = state.pulse.create(new).map_err(failure)?;
    Ok(Json(json!({"reminder": reminder})))
}

async fn acknowledge(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&state, &headers)?;
    let reminder = state.pulse.acknowledge(&id).map_err(failure)?;
    Ok(Json(json!({"reminder": reminder})))
}

async fn cancel(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&state, &headers)?;
    let reminder = state.pulse.cancel(&id).map_err(failure)?;
    Ok(Json(json!({"reminder": reminder})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnoozeRequest {
    minutes: Option<u32>,
    until: Option<String>,
}

async fn snooze(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SnoozeRequest>,
) -> Reply {
    authorize(&state, &headers)?;
    let until = match (request.minutes, request.until) {
        (Some(minutes), None) if (1..=10_080).contains(&minutes) => {
            Utc::now() + Duration::minutes(minutes as i64)
        }
        (Some(_), None) => return Err(invalid("minutes must be between 1 and 10080")),
        (None, Some(until)) => parse_due(&until)?,
        _ => return Err(invalid("provide either minutes or until, not both")),
    };
    let reminder = state.pulse.snooze(&id, until).map_err(failure)?;
    Ok(Json(json!({"reminder": reminder})))
}

async fn settings(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    Ok(Json(
        json!({"settings": state.pulse.settings().map_err(failure)?}),
    ))
}

async fn update_settings(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(settings): Json<PulseSettings>,
) -> Reply {
    authorize(&state, &headers)?;
    let settings = state.pulse.update_settings(&settings).map_err(failure)?;
    Ok(Json(json!({"settings": settings})))
}
