//! Arena Learn Interactive Courses owner API (#386), merged into `/api/learn`.
//!
//! No route here calls Mind or any model. Grades come from deterministic graders or the
//! isolated `sail-runner`. If the runner isn't configured, Rust steps report `unavailable`;
//! there is no host `rustc` fallback.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use sail_arena_learn::courses::content::RunLimits;
use sail_arena_learn::courses::grade::{RustRun, parse_rustc_json};
use sail_arena_learn::courses::{
    AttemptRequest, Context, CourseEngine, CourseError, RustEvaluator,
};
use sail_guardian::authority::ApprovalAuthority;
use sail_protocol::execution::{CommandLimits, CommandPlan, CommandStatus};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct CoursesState {
    engine: Arc<CourseEngine>,
    authority: Option<ApprovalAuthority>,
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

pub fn router(engine: Arc<CourseEngine>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/courses", get(courses))
        .route("/courses/:id", get(course))
        .route("/next", get(next))
        .route("/lessons/:id", get(lesson))
        .route("/lessons/:id/attempt", post(lesson_attempt))
        .route("/lessons/:id/hint", post(lesson_hint))
        .route("/lessons/:id/reveal", post(lesson_reveal))
        .route("/lessons/:id/advance", post(advance))
        .route("/diagnostics", post(diagnostic_start))
        .route("/diagnostics/:id", get(diagnostic_view))
        .route("/diagnostics/:id/attempt", post(diagnostic_attempt))
        .route("/diagnostics/:id/report", get(diagnostic_report))
        .route("/reviews/due", get(reviews_due))
        .route("/reviews/:id/attempt", post(review_attempt))
        .route("/reviews/:id/hint", post(review_hint))
        .route("/skills/evidence", get(evidence))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(Arc::new(CoursesState { engine, authority }))
}

fn authorize(s: &CoursesState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
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

fn reply(r: Result<Value, CourseError>) -> Reply {
    r.map(Json).map_err(|e| {
        let code = match &e {
            CourseError::NotFound(_) => StatusCode::NOT_FOUND,
            CourseError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CourseError::Conflict(_) => StatusCode::CONFLICT,
            CourseError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if code == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::warn!(error = %e, "Arena Learn course storage error");
            return (
                code,
                Json(json!({"error":"Course progress storage is unavailable."})),
            );
        }
        (code, Json(json!({"error": e.to_string()})))
    })
}

#[derive(Deserialize)]
struct CourseQuery {
    course: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepBody {
    #[serde(default)]
    step_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    course_id: String,
}

async fn courses(State(s): State<Arc<CoursesState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.courses())
}

async fn course(State(s): State<Arc<CoursesState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.course(&id))
}

async fn next(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Query(q): Query<CourseQuery>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.next(&q.course))
}

async fn lesson(State(s): State<Arc<CoursesState>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.lesson_view(&id))
}

async fn lesson_attempt(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<AttemptRequest>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.lesson_attempt(&id, req).await)
}

async fn lesson_hint(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<StepBody>,
) -> Reply {
    authorize(&s, &h)?;
    reply(
        s.engine
            .hint(Context::Lesson, &id, b.step_id.as_deref())
            .await,
    )
}

async fn lesson_reveal(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<StepBody>,
) -> Reply {
    authorize(&s, &h)?;
    let step = b.step_id.unwrap_or_default();
    reply(s.engine.reveal(&id, &step).await)
}

async fn advance(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.advance(&id).await)
}

async fn diagnostic_start(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Json(b): Json<StartBody>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.diagnostic_start(&b.course_id).await)
}

async fn diagnostic_attempt(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<AttemptRequest>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.diagnostic_attempt(&id, req).await)
}

async fn diagnostic_view(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.diagnostic_view(&id))
}

async fn diagnostic_report(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.diagnostic_report(&id))
}

async fn reviews_due(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Query(q): Query<CourseQuery>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.reviews_due(&q.course))
}

async fn review_attempt(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<AttemptRequest>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.review_attempt(&id, req).await)
}

async fn review_hint(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.hint(Context::Review, &id, None).await)
}

async fn evidence(
    State(s): State<Arc<CoursesState>>,
    h: HeaderMap,
    Query(q): Query<CourseQuery>,
) -> Reply {
    authorize(&s, &h)?;
    reply(s.engine.evidence(&q.course))
}

/// Exit code the in-image checker uses to say "rustc rejected the program".
const COMPILE_FAILED_EXIT: i32 = 86;

/// Compiles and runs learner Rust through the dedicated-UID `sail-runner` service. It needs
/// `SAIL_RUNNER_SOCKET`, `SAIL_RUNNER_UID` and a checker installed in the runner image at
/// `SAIL_LEARN_RUST_CHECKER` (default `/usr/local/bin/sail-learn-rust-check`). See
/// `docs/SAIL_ARENA_LEARN_COURSES.md`.
pub struct RunnerRustEvaluator {
    socket: std::path::PathBuf,
    uid: u32,
    checker: String,
}

impl RunnerRustEvaluator {
    /// `None` when the runner isn't configured; the caller then uses `NoSandbox`.
    pub fn from_env() -> Option<Self> {
        let socket = std::env::var_os("SAIL_RUNNER_SOCKET")?.into();
        let uid = std::env::var("SAIL_RUNNER_UID").ok()?.parse().ok()?;
        let checker = std::env::var("SAIL_LEARN_RUST_CHECKER")
            .unwrap_or_else(|_| "/usr/local/bin/sail-learn-rust-check".into());
        Some(Self {
            socket,
            uid,
            checker,
        })
    }
}

#[async_trait::async_trait]
impl RustEvaluator for RunnerRustEvaluator {
    async fn run(&self, source: &str, limits: RunLimits) -> RustRun {
        let plan = CommandPlan {
            version: 1,
            request_id: uuid::Uuid::new_v4(),
            correlation_id: uuid::Uuid::new_v4(),
            executable: self.checker.clone(),
            argv: vec![source.to_string()],
            limits: CommandLimits {
                wall_ms: u64::from(limits.wall_ms).clamp(1_000, 60_000),
                memory_bytes: u64::from(limits.mem_mb).clamp(64, 1024) * 1024 * 1024,
                ..CommandLimits::default()
            },
        };
        if let Err(e) = plan.validate() {
            return RustRun::Unavailable(format!("The run was refused: {e}."));
        }
        let socket = self.socket.clone();
        let uid = self.uid;
        let result = tokio::task::spawn_blocking(move || {
            sail_runner::dispatch_authorized(
                &socket,
                uid,
                &plan,
                tokio_util::sync::CancellationToken::new(),
            )
        })
        .await;
        let result = match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                return RustRun::Unavailable(format!("The Rust sandbox couldn't be reached: {e}."));
            }
            Err(_) => {
                return RustRun::Unavailable("The Rust sandbox job stopped unexpectedly.".into());
            }
        };
        let diagnostics = parse_rustc_json(&result.stderr);
        match (result.status, result.exit_code) {
            (CommandStatus::Completed, _) => RustRun::Compiled {
                exit_code: Some(0),
                stdout: result.stdout,
                diagnostics,
            },
            (CommandStatus::Failed, Some(COMPILE_FAILED_EXIT)) => {
                RustRun::CompileFailed { diagnostics }
            }
            (CommandStatus::Failed, Some(code)) => RustRun::Compiled {
                exit_code: Some(code),
                stdout: result.stdout,
                diagnostics,
            },
            (status, _) => RustRun::Unavailable(format!(
                "The Rust sandbox didn't finish the run ({}).",
                serde_json::to_value(status)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
            )),
        }
    }
}
