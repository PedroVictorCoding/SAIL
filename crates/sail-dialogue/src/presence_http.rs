//! Owner-only presence endpoints, nested at `/api/presence`.
//!
//! Clients report metadata-only heartbeats; the server stamps them with its own clock.
//! Quiet hours come from Pulse settings when a Pulse store is passed, so presence and
//! reminders agree about the owner's night. Firing `PresenceEvent`s is the daemon's job:
//! these endpoints return state only.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::Utc;
use sail_guardian::authority::ApprovalAuthority;
use sail_pulse::Pulse;
use sail_pulse::presence::store::PresenceStore;
use sail_pulse::presence::{Heartbeat, PresenceSettings, PresenceState};
use serde_json::{Value, json};
use std::sync::Arc;

/// Largest heartbeat or settings body accepted, in bytes (fixed a priori: these are tiny
/// metadata objects; anything larger is hostile or a bug).
const MAX_BODY_BYTES: usize = 4 * 1024;

struct HttpState {
    presence: Arc<PresenceStore>,
    pulse: Option<Arc<Pulse>>,
    authority: Option<ApprovalAuthority>,
}

/// Presence routes. Nest at `/api/presence`; the index route answers `GET /api/presence`.
pub fn router(
    presence: Arc<PresenceStore>,
    pulse: Option<Arc<Pulse>>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    Router::new()
        .route("/", get(state))
        .route("/heartbeat", post(heartbeat))
        .route("/settings", get(settings).post(update_settings))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(HttpState {
            presence,
            pulse,
            authority,
        }))
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
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| refuse(StatusCode::UNAUTHORIZED))?;
    if !authority.authenticates(token) {
        return Err(refuse(StatusCode::UNAUTHORIZED));
    }
    Ok(())
}

/// Quiet hours according to Pulse, or false when presence runs without one.
fn quiet_hours(state: &HttpState, now: chrono::DateTime<Utc>) -> bool {
    state
        .pulse
        .as_ref()
        .and_then(|pulse| pulse.settings().ok())
        .is_some_and(|settings| settings.is_quiet_at(now))
}

fn bad_request(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
}

async fn state(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    let now = Utc::now();
    let presence: PresenceState = state.presence.state(now, quiet_hours(&state, now));
    Ok(Json(json!(presence)))
}

async fn heartbeat(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(beat): Json<Heartbeat>,
) -> Reply {
    authorize(&state, &headers)?;
    let now = Utc::now();
    let quiet = quiet_hours(&state, now);
    let (presence, _events) = state
        .presence
        .record(beat, now, quiet)
        .map_err(bad_request)?;
    Ok(Json(json!({"state": presence})))
}

async fn settings(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    Ok(Json(json!(state.presence.settings())))
}

async fn update_settings(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(settings): Json<PresenceSettings>,
) -> Reply {
    authorize(&state, &headers)?;
    let saved = state
        .presence
        .save_settings(settings)
        .map_err(bad_request)?;
    Ok(Json(json!(saved)))
}
