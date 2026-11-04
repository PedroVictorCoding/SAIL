//! Arena Learn owner API (#246), nested at `/api/learn`.
//!
//! Every route needs the owner bearer token. Turns run in the background: a request starts
//! one and returns; the live view comes from `GET /sessions/:id/stream` (Server-Sent
//! Events, read with `fetch` so the token travels in a header). A disconnect detaches the
//! view and leaves the turn running; `POST …/abort` is the only way to stop it.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use futures_util::stream::{self, Stream};
use sail_arena_learn::core::question::Answer;
use sail_arena_learn::{Engine, StoreError, TurnError};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

struct LearnState {
    engine: Arc<Engine>,
    authority: Option<ApprovalAuthority>,
}

type Reply = Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)>;

pub fn router(engine: Arc<Engine>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/sessions", get(list).post(create))
        .route("/sessions/:id", get(view).delete(remove))
        .route("/sessions/:id/turn", post(turn))
        .route("/sessions/:id/answer", post(answer))
        .route("/sessions/:id/message", post(message))
        .route("/sessions/:id/abort", post(abort))
        .route("/sessions/:id/pause", post(pause))
        .route("/sessions/:id/stream", get(stream_view))
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
        .with_state(Arc::new(LearnState { engine, authority }))
}

fn authorize(s: &LearnState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
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

fn error(e: TurnError) -> (StatusCode, Json<Value>) {
    let code = match &e {
        TurnError::Busy | TurnError::QuestionOpen | TurnError::Paused => StatusCode::CONFLICT,
        TurnError::Store(StoreError::Conflict { .. }) => StatusCode::CONFLICT,
        TurnError::Invalid(_) => StatusCode::BAD_REQUEST,
        TurnError::Store(StoreError::NotFound(_)) => StatusCode::NOT_FOUND,
        TurnError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if code == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::warn!(error=%e, "Arena Learn storage error");
        return (
            code,
            Json(json!({"error":"Learning storage is unavailable."})),
        );
    }
    (
        code,
        Json(json!({"error":e.to_string(),"busy":code==StatusCode::CONFLICT})),
    )
}

fn ok(code: StatusCode, value: Value) -> Reply {
    Ok((code, Json(value)))
}

fn snapshot(engine: &Engine, id: &str) -> Result<Value, TurnError> {
    let view = engine.view(id)?;
    let mut value = serde_json::to_value(view).map_err(|e| TurnError::Invalid(e.to_string()))?;
    value["running"] = json!(engine.is_running(id));
    Ok(value)
}

/// Starts a turn if the model owes one. Busy is fine: a turn is already doing it.
fn continue_session(engine: &Arc<Engine>, id: &str) {
    if engine.view(id).is_ok_and(|v| v.needs_turn)
        && let Err(e) = engine.start_turn(id)
        && !matches!(e, TurnError::Busy)
    {
        tracing::debug!(error=%e, "Arena Learn turn not started");
    }
}

async fn list(State(s): State<Arc<LearnState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let sessions = s.engine.sessions(100).map_err(error)?;
    ok(StatusCode::OK, json!({"sessions":sessions}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    goal: String,
}

async fn create(State(s): State<Arc<LearnState>>, h: HeaderMap, Json(c): Json<Create>) -> Reply {
    authorize(&s, &h)?;
    let view = s.engine.create_session(&c.goal).map_err(error)?;
    continue_session(&s.engine, &view.session);
    ok(
        StatusCode::CREATED,
        snapshot(&s.engine, &view.session).map_err(error)?,
    )
}

async fn view(State(s): State<Arc<LearnState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    ok(StatusCode::OK, snapshot(&s.engine, &id).map_err(error)?)
}

async fn remove(State(s): State<Arc<LearnState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    let deleted = s.engine.delete(&id).map_err(error)?;
    ok(
        if deleted {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        },
        json!({"deleted":deleted}),
    )
}

async fn turn(State(s): State<Arc<LearnState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    s.engine.start_turn(&id).map_err(error)?;
    ok(StatusCode::ACCEPTED, json!({"started":true}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerBody {
    question_id: String,
    answer: Answer,
}

async fn answer(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<AnswerBody>,
) -> Reply {
    authorize(&s, &h)?;
    // No busy check: answers are idempotent, and a question is only open once its turn
    // has settled. If the follow-up turn races the claim release, the client sees
    // `needs_turn && !running` in the next view and starts it.
    let outcome = s
        .engine
        .answer(&id, &b.question_id, b.answer)
        .map_err(error)?;
    continue_session(&s.engine, &id);
    ok(StatusCode::OK, json!(outcome))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageBody {
    key: String,
    text: String,
}

async fn message(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<MessageBody>,
) -> Reply {
    authorize(&s, &h)?;
    s.engine.message(&id, &b.key, &b.text).map_err(error)?;
    continue_session(&s.engine, &id);
    ok(StatusCode::ACCEPTED, json!({"accepted":true}))
}

async fn abort(State(s): State<Arc<LearnState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    ok(StatusCode::OK, json!({"aborted":s.engine.abort(&id)}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseBody {
    paused: bool,
}

async fn pause(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<PauseBody>,
) -> Reply {
    authorize(&s, &h)?;
    s.engine.set_paused(&id, b.paused).map_err(error)?;
    if !b.paused {
        continue_session(&s.engine, &id);
    }
    ok(StatusCode::OK, snapshot(&s.engine, &id).map_err(error)?)
}

/// Sends the full view on connect and again after every change to this session. Views
/// are small, so a reconnect needs no replay cursor and can never double-apply anything.
async fn stream_view(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    authorize(&s, &h)?;
    let first = snapshot(&s.engine, &id).map_err(error)?;
    let updates = s.engine.subscribe();
    let engine = s.engine.clone();
    let events = stream::unfold(
        (Some(first), updates, engine, id),
        |(pending, mut updates, engine, id)| async move {
            if let Some(view) = pending {
                let event = Event::default().event("view").data(view.to_string());
                return Some((Ok(event), (None, updates, engine, id)));
            }
            loop {
                match updates.recv().await {
                    Ok(update) if update.session != id => continue,
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        let event = match snapshot(&engine, &id) {
                            Ok(view) => Event::default().event("view").data(view.to_string()),
                            Err(_) => Event::default().event("gone").data("{}"),
                        };
                        return Some((Ok(event), (None, updates, engine, id)));
                    }
                    Err(RecvError::Closed) => return None,
                }
            }
        },
    );
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}
