//! Owner-only initiative and escalation routes, nested at `/api/initiative` and
//! `/api/mind/escalation`. Same owner-token rule as `/api/pulse`.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::Utc;
use sail_guardian::authority::ApprovalAuthority;
use sail_mind::escalation::Outcome;
use sail_pulse::PulseError;
use sail_pulse::attention::{AttentionSettings, Feedback};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::proactive::Proactive;

struct HttpState {
    proactive: Arc<Proactive>,
    authority: Option<ApprovalAuthority>,
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn no_store(router: Router) -> Router {
    router
        .layer(axum::extract::DefaultBodyLimit::max(32_000))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
}

/// `/api/initiative`.
pub fn initiative_router(
    proactive: Arc<Proactive>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    no_store(
        Router::new()
            .route("/inbox", get(inbox))
            .route("/deliverable", get(deliverable))
            .route("/items/:id/feedback", post(feedback))
            .route("/digest", get(digest))
            .route("/digest/release", post(release_digest))
            .route("/settings", get(settings).post(save_settings))
            .route("/stats", get(stats))
            .route("/agenda", get(agenda))
            .route("/tick", post(tick))
            .with_state(Arc::new(HttpState {
                proactive,
                authority,
            })),
    )
}

/// `/api/mind/escalation`.
pub fn escalation_router(
    proactive: Arc<Proactive>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    no_store(
        Router::new()
            .route("/status", get(escalation_status))
            .route("/jobs", get(escalation_jobs))
            .route("/jobs/:id/outcome", post(escalation_outcome))
            .with_state(Arc::new(HttpState {
                proactive,
                authority,
            })),
    )
}

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

fn internal(message: impl ToString) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":message.to_string()})),
    )
}

fn to_json<T: serde::Serialize>(value: T) -> Reply {
    serde_json::to_value(value).map(Json).map_err(internal)
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
}

async fn inbox(
    State(s): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(q): Query<Limit>,
) -> Reply {
    authorize(&s, &headers)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    to_json(
        s.proactive
            .initiative
            .attention()
            .inbox(limit)
            .map_err(failure)?,
    )
}

async fn deliverable(State(s): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&s, &headers)?;
    to_json(
        s.proactive
            .initiative
            .attention()
            .deliverable(Utc::now())
            .map_err(failure)?,
    )
}

async fn feedback(
    State(s): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(feedback): Json<Feedback>,
) -> Reply {
    authorize(&s, &headers)?;
    let now = Utc::now();
    if let Feedback::Later(until) = feedback
        && (until <= now || until > now + chrono::Duration::days(60))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"later must be within the next 60 days"})),
        ));
    }
    to_json(
        s.proactive
            .initiative
            .attention()
            .feedback(&id, feedback, now)
            .map_err(failure)?,
    )
}

async fn digest(State(s): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&s, &headers)?;
    to_json(s.proactive.last_digest())
}

async fn release_digest(State(s): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&s, &headers)?;
    let batch = s
        .proactive
        .initiative
        .attention()
        .release_digest(Utc::now())
        .map_err(failure)?;
    if batch.is_some() {
        s.proactive.set_last_digest(batch.clone());
    }
    to_json(batch)
}

async fn settings(State(s): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&s, &headers)?;
    to_json(
        s.proactive
            .initiative
            .attention()
            .settings()
            .map_err(failure)?,
    )
}

async fn save_settings(
    State(s): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(settings): Json<AttentionSettings>,
) -> Reply {
    authorize(&s, &headers)?;
    let thresholds = [
        settings.interrupt_threshold,
        settings.suggest_threshold,
        settings.digest_threshold,
        settings.min_novelty,
    ];
    if thresholds
        .iter()
        .any(|t| !t.is_finite() || !(0.0..=1.0).contains(t))
        || settings.max_interrupts_per_hour > 30
        || settings.max_suggestions_per_day > 100
        || settings.digest_minutes.len() > 6
        || settings.digest_minutes.iter().any(|m| *m >= 24 * 60)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"settings out of range"})),
        ));
    }
    s.proactive
        .initiative
        .attention()
        .save_settings(&settings)
        .map_err(failure)?;
    to_json(settings)
}

async fn stats(State(s): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&s, &headers)?;
    let attention = s
        .proactive
        .initiative
        .attention()
        .stats(Utc::now())
        .map_err(failure)?;
    Ok(Json(
        json!({"attention": attention, "last_tick": s.proactive.last_tick()}),
    ))
}

async fn agenda(
    State(s): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(q): Query<Limit>,
) -> Reply {
    authorize(&s, &headers)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    to_json(
        s.proactive
            .initiative
            .agenda()
            .open_items(limit)
            .map_err(internal)?,
    )
}

/// Run one tick now (for the owner's "check now", and for tests).
async fn tick(State(s): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&s, &headers)?;
    let proactive = s.proactive.clone();
    let report = tokio::task::spawn_blocking(move || proactive.tick_now())
        .await
        .map_err(internal)?;
    to_json(report)
}

fn escalation(
    s: &HttpState,
) -> Result<&Arc<sail_mind::escalation::service::EscalationService>, (StatusCode, Json<Value>)> {
    s.proactive.escalation.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error":"escalation is unavailable"})),
    ))
}

async fn escalation_status(State(s): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&s, &headers)?;
    to_json(escalation(&s)?.status().map_err(internal)?)
}

async fn escalation_jobs(
    State(s): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(q): Query<Limit>,
) -> Reply {
    authorize(&s, &headers)?;
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    to_json(escalation(&s)?.store().jobs(limit).map_err(internal)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeBody {
    outcome: Outcome,
}

async fn escalation_outcome(
    State(s): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<OutcomeBody>,
) -> Reply {
    authorize(&s, &headers)?;
    escalation(&s)?
        .feedback(&id, body.outcome)
        .map_err(|e| (StatusCode::NOT_FOUND, Json(json!({"error": e}))))?;
    Ok(Json(json!({})))
}
