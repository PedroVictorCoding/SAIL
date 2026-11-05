//! Arena Learn owner API (#246), nested at `/api/learn`.
//!
//! Every route needs the owner bearer token. Turns run in the background: a request starts
//! one and returns; the live view comes from `GET /sessions/:id/stream` (Server-Sent
//! Events, read with `fetch` so the token travels in a header). A disconnect detaches the
//! view and leaves the turn running; `POST …/abort` is the only way to stop it.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    routing::{delete, get, post},
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
    pulse: Option<Arc<sail_pulse::Pulse>>,
}

type Reply = Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)>;

pub fn router(engine: Arc<Engine>, authority: Option<ApprovalAuthority>) -> Router {
    router_with_pulse(engine, authority, None)
}

pub fn router_with_pulse(
    engine: Arc<Engine>,
    authority: Option<ApprovalAuthority>,
    pulse: Option<Arc<sail_pulse::Pulse>>,
) -> Router {
    Router::new()
        .route("/prompts", get(prompts))
        .route(
            "/sources",
            get(list_sources)
                .post(upload_source)
                .layer(DefaultBodyLimit::max(5 * 1024 * 1024 + 64_000)),
        )
        .route("/sources/:id", delete(remove_source))
        .route("/sessions", get(list).post(create))
        .route("/sessions/:id", get(view).delete(remove))
        .route("/sessions/:id/turn", post(turn))
        .route("/sessions/:id/answer", post(answer))
        .route("/sessions/:id/hint", post(hint))
        .route("/sessions/:id/attempt", post(attempt))
        .route("/sessions/:id/diagram-error", post(diagram_error))
        .route(
            "/sessions/:id/review",
            post(schedule_review).delete(cancel_review),
        )
        .route("/sessions/:id/message", post(message))
        .route("/sessions/:id/abort", post(abort))
        .route("/sessions/:id/pause", post(pause))
        .route("/sessions/:id/stream", get(stream_view))
        .layer(axum::extract::DefaultBodyLimit::max(
            5 * 1024 * 1024 + 64_000,
        ))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(LearnState {
            engine,
            authority,
            pulse,
        }))
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
    /// `learn` (default) or `practice` (#244).
    #[serde(default)]
    mode: Option<String>,
}

async fn create(State(s): State<Arc<LearnState>>, h: HeaderMap, Json(c): Json<Create>) -> Reply {
    authorize(&s, &h)?;
    let view = match c.mode.as_deref() {
        None | Some("learn") => s.engine.create_session(&c.goal),
        Some("practice") => s.engine.create_practice_session(&c.goal),
        Some(other) => Err(TurnError::Invalid(format!(
            "mode must be `learn` or `practice`, not `{other}`"
        ))),
    }
    .map_err(error)?;
    continue_session(&s.engine, &view.session);
    ok(
        StatusCode::CREATED,
        snapshot(&s.engine, &view.session).map_err(error)?,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceQuery {
    #[serde(default)]
    topic: Option<String>,
}

async fn list_sources(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Query(q): Query<SourceQuery>,
) -> Reply {
    authorize(&s, &h)?;
    let sources = s.engine.sources(q.topic.as_deref()).map_err(error)?;
    let propositions = s
        .engine
        .proposition_count(q.topic.as_deref())
        .map_err(error)?;
    ok(
        StatusCode::OK,
        json!({"sources":sources,"propositions":propositions}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadSource {
    topic: String,
    name: String,
    content: String,
}

/// Owner upload of a Markdown/text note (#250). The content is capped at 5 MiB.
async fn upload_source(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Json(b): Json<UploadSource>,
) -> Reply {
    authorize(&s, &h)?;
    let name = b.name.trim();
    if !(name.ends_with(".md") || name.ends_with(".txt")) {
        return Err(error(TurnError::Invalid(
            "only .md and .txt notes can be imported".into(),
        )));
    }
    if b.content.len() > 5 * 1024 * 1024 {
        return Err(error(TurnError::Invalid(
            "a note may be at most 5 MiB".into(),
        )));
    }
    let source = s
        .engine
        .import_notes(&b.topic, name, &b.content)
        .map_err(error)?;
    // Extraction runs in the background; the source is visible immediately.
    let engine = s.engine.clone();
    let topic = b.topic.clone();
    let content = b.content.clone();
    let source_name = source.name.clone();
    let id = source.id;
    tokio::spawn(async move {
        if let Err(error) = engine
            .extract_source(&topic, id, &source_name, &content)
            .await
        {
            tracing::warn!(%error, source = id, "Arena Learn note extraction failed");
        }
    });
    ok(StatusCode::ACCEPTED, json!({"source":source}))
}

async fn remove_source(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> Reply {
    authorize(&s, &h)?;
    s.engine.delete_source(id).map_err(error)?;
    ok(StatusCode::OK, json!({"deleted":true}))
}

async fn prompts(State(s): State<Arc<LearnState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let prompts: serde_json::Map<String, Value> = s
        .engine
        .effective_prompts()
        .into_iter()
        .map(|(name, text)| (name, Value::String(text)))
        .collect();
    ok(StatusCode::OK, json!({"prompts":prompts}))
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
struct HintBody {
    problem_id: String,
    index: u32,
}

async fn hint(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<HintBody>,
) -> Reply {
    authorize(&s, &h)?;
    let view = s
        .engine
        .request_hint(&id, &b.problem_id, b.index)
        .map_err(error)?;
    ok(
        StatusCode::OK,
        serde_json::to_value(view).map_err(|e| error(TurnError::Invalid(e.to_string())))?,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttemptBody {
    problem_id: String,
    text: String,
}

async fn attempt(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<AttemptBody>,
) -> Reply {
    authorize(&s, &h)?;
    s.engine
        .attempt(&id, &b.problem_id, &b.text)
        .map_err(error)?;
    continue_session(&s.engine, &id);
    ok(StatusCode::ACCEPTED, json!({"accepted":true}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagramErrorBody {
    call_id: String,
    message: String,
}

async fn diagram_error(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<DiagramErrorBody>,
) -> Reply {
    authorize(&s, &h)?;
    s.engine
        .diagram_failed(&id, &b.call_id, &b.message)
        .map_err(error)?;
    ok(StatusCode::ACCEPTED, json!({"reported":true}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewBody {
    days: u8,
    #[serde(default)]
    problem_id: Option<String>,
    timezone: String,
    utc_offset_minutes: i32,
}

/// Schedules one opt-in Pulse reminder after practice (#257). Off unless this is called.
async fn schedule_review(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<ReviewBody>,
) -> Reply {
    authorize(&s, &h)?;
    if !matches!(b.days, 1 | 3 | 7) {
        return Err(error(TurnError::Invalid("choose 1, 3 or 7 days".into())));
    }
    let pulse = s.pulse.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"reminders are unavailable"})),
        )
    })?;
    let view = s.engine.view(&id).map_err(error)?;
    let due = chrono::Utc::now() + chrono::Duration::days(i64::from(b.days));
    let text = format!("Practise: {}", view.goal);
    let reminder = pulse
        .create(
            sail_pulse::NewReminder::new(text, due, b.timezone, b.utc_offset_minutes)
                .map_err(|e| error(TurnError::Invalid(e.to_string())))?,
        )
        .map_err(|e| error(TurnError::Invalid(e.to_string())))?;
    let updated = s
        .engine
        .schedule_review(
            &id,
            b.problem_id,
            b.days,
            reminder.due_at.timestamp_millis(),
            &reminder.id,
        )
        .map_err(error)?;
    ok(
        StatusCode::CREATED,
        serde_json::to_value(updated).map_err(|e| error(TurnError::Invalid(e.to_string())))?,
    )
}

async fn cancel_review(
    State(s): State<Arc<LearnState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    let reminder = s.engine.cancel_review(&id).map_err(error)?;
    if let (Some(pulse), Some(reminder)) = (s.pulse.as_ref(), reminder.as_deref()) {
        let _ = pulse.cancel(reminder);
    }
    ok(StatusCode::OK, json!({"cancelled":reminder.is_some()}))
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
