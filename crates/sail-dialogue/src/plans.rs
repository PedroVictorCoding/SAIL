//! Agent task plans in Dialogue (`docs/SAIL_AGENT_PLANNING.md`): the plan runner over
//! Dialogue's tool registry, "Plan: …" turns, and the owner's plan controls.
//!
//! Anyone who can reach the voice socket can start, view or cancel a plan, as with any
//! voice turn. Approving or editing a plan, which lets its effects run, requires the owner
//! token over HTTP, like Guardian approvals.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use sail_agents::planning::{
    MindPlanner, PlanDraft, PlanError, PlanRunner, PlanState, PlanView, Planner,
};
use sail_common::types::SubsystemId;
use sail_continuity::Continuity;
use sail_guardian::authority::ApprovalAuthority;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// The plan runner and the plans this process is executing.
pub struct Plans {
    runner: Arc<PlanRunner>,
    scheduler: Arc<sail_agents::tools::ToolScheduler>,
    running: Mutex<HashSet<String>>,
}

impl Plans {
    /// Plans over Dialogue's shared tool scheduler (Guardian checks every step), drafted by
    /// the resident local model.
    pub fn with_mind(
        continuity: Arc<Continuity>,
        mind: sail_mind::MindService,
    ) -> Result<Arc<Self>, sail_guardian::ExecutionDenied> {
        Self::with_planner(continuity, Arc::new(MindPlanner::new(mind)))
    }

    pub fn with_planner(
        continuity: Arc<Continuity>,
        planner: Arc<dyn Planner>,
    ) -> Result<Arc<Self>, sail_guardian::ExecutionDenied> {
        let scheduler = crate::execution::shared()?;
        let registry = scheduler.registry();
        Ok(Arc::new(Self {
            runner: Arc::new(PlanRunner::new(
                continuity,
                scheduler.clone(),
                registry,
                planner,
                SubsystemId::Agents,
            )),
            scheduler,
            running: Mutex::new(HashSet::new()),
        }))
    }

    /// After a restart, run every plan that was running. Call after the store's recovery
    /// pass (Forge's start), which marks the previous boot's tasks interrupted.
    pub fn resume(self: &Arc<Self>) -> Result<usize, PlanError> {
        let ids = self.runner.resume_all()?;
        for id in &ids {
            info!(plan_id = %id, "Resuming plan after restart");
            self.spawn(id);
        }
        Ok(ids.len())
    }

    /// Run the plan in the background unless this process already is. Only the owner's
    /// cancel stops it; at shutdown it is left running and resumes on the next start.
    fn spawn(self: &Arc<Self>, id: &str) {
        if !self.running.lock().unwrap().insert(id.into()) {
            return;
        }
        let plans = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            match plans.runner.run(&id, CancellationToken::new()).await {
                Ok(plan) => info!(plan_id = %id, state = ?plan.state, "Plan run stopped"),
                Err(error) => warn!(plan_id = %id, %error, "Plan run failed"),
            }
            plans.running.lock().unwrap().remove(&id);
        });
    }

    /// Plan a goal. A plan with no effects starts at once; one with effects waits for the
    /// owner's approval.
    pub async fn create(self: &Arc<Self>, goal: &str) -> Result<PlanView, PlanError> {
        let plan = self.runner.create(goal).await?;
        if plan.state == PlanState::Running {
            self.spawn(&plan.id);
        }
        self.runner.get(&plan.id)
    }

    pub fn approve(self: &Arc<Self>, id: &str) -> Result<PlanView, PlanError> {
        let plan = self.runner.approve(id)?;
        self.spawn(&plan.id);
        self.runner.get(id)
    }

    pub fn edit(&self, id: &str, draft: &PlanDraft) -> Result<PlanView, PlanError> {
        self.runner.edit(id, draft)?;
        self.runner.get(id)
    }

    pub fn cancel(&self, id: &str, reason: &str) -> Result<PlanView, PlanError> {
        self.runner.cancel(id, reason)?;
        self.runner.get(id)
    }

    /// For a plan that needs the owner's attention: retry the interrupted step (the owner
    /// checked it did not happen) or cancel the plan.
    pub fn resolve(self: &Arc<Self>, id: &str, retry: bool) -> Result<PlanView, PlanError> {
        let view = self.runner.resolve(id, retry)?;
        if retry {
            self.spawn(id);
        }
        Ok(view)
    }

    pub fn get(&self, id: &str) -> Result<PlanView, PlanError> {
        self.runner.get(id)
    }

    /// Events of the tool calls plans make, including Guardian approval requests.
    pub fn tool_events(
        &self,
    ) -> tokio::sync::broadcast::Receiver<sail_protocol::execution::ToolEvent> {
        self.scheduler.subscribe()
    }

    pub fn list(&self, limit: usize) -> Result<Vec<PlanView>, PlanError> {
        self.runner.list(limit)
    }
}

/// The goal of a turn that asks for a plan: "Plan: …", "Plan, …" (as speech recognition
/// writes it) or "Plan - …". Ordinary sentences such as "plan a trip" stay conversation.
pub fn requested_goal(prompt: &str) -> Option<&str> {
    let text = prompt.trim_start();
    let head = text.get(..4)?;
    if !head.eq_ignore_ascii_case("plan") {
        return None;
    }
    let rest = text[4..].trim_start();
    let goal = rest.strip_prefix([':', ',', '-', '.'])?.trim();
    (!goal.is_empty()).then_some(goal)
}

/// What SAIL says after planning a goal: short, and it points at the plan card.
pub fn spoken_summary(view: &PlanView) -> String {
    let plan = &view.plan;
    let steps = match plan.steps.len() {
        1 => "one step".to_owned(),
        n => format!("{n} steps"),
    };
    match plan.state {
        PlanState::AwaitingApproval => format!(
            "I planned that in {steps}. It changes something, so it is waiting for your approval in the plan card."
        ),
        PlanState::Running => {
            format!("I planned that in {steps} and started it. Progress is in the plan card.")
        }
        _ => format!("I planned that in {steps}."),
    }
}

struct HttpState {
    plans: Arc<Plans>,
    authority: Option<ApprovalAuthority>,
}

/// Owner-only plan controls, nested at `/api/plans`.
pub fn router(plans: Arc<Plans>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/", get(list).post(create))
        .route("/:id", get(show))
        .route("/:id/approve", post(approve))
        .route("/:id/edit", post(edit))
        .route("/:id/cancel", post(cancel))
        .route("/:id/resolve", post(resolve))
        .layer(axum::extract::DefaultBodyLimit::max(64000))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(HttpState { plans, authority }))
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn authorize(state: &HttpState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let refuse = |status: StatusCode| (status, Json(json!({"error":"owner token required"})));
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

fn failure(error: PlanError) -> (StatusCode, Json<Value>) {
    let status = match &error {
        PlanError::Planning(_)
        | PlanError::NotAwaitingApproval(_)
        | PlanError::Continuity(sail_continuity::ContinuityError::InvalidState { .. }) => {
            StatusCode::CONFLICT
        }
        PlanError::Continuity(sail_continuity::ContinuityError::NotFound(_)) => {
            StatusCode::NOT_FOUND
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({"error":error.to_string()})))
}

fn view(view: PlanView) -> Json<Value> {
    Json(json!({"plan":view}))
}

async fn list(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    let plans = state.plans.list(20).map_err(failure)?;
    Ok(Json(json!({"plans":plans})))
}

async fn show(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&state, &headers)?;
    state.plans.get(&id).map(view).map_err(failure)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    goal: String,
}

async fn create(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(request): Json<CreateRequest>,
) -> Reply {
    authorize(&state, &headers)?;
    state
        .plans
        .create(&request.goal)
        .await
        .map(view)
        .map_err(failure)
}

async fn approve(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply {
    authorize(&state, &headers)?;
    state.plans.approve(&id).map(view).map_err(failure)
}

async fn edit(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(draft): Json<PlanDraft>,
) -> Reply {
    authorize(&state, &headers)?;
    state.plans.edit(&id, &draft).map(view).map_err(failure)
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CancelRequest {
    #[serde(default)]
    reason: Option<String>,
}

async fn cancel(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    request: Option<Json<CancelRequest>>,
) -> Reply {
    authorize(&state, &headers)?;
    let reason = request
        .and_then(|Json(r)| r.reason)
        .unwrap_or_else(|| "cancelled by the owner".into());
    let reason: String = reason.chars().take(300).collect();
    state.plans.cancel(&id, &reason).map(view).map_err(failure)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {
    retry: bool,
}

async fn resolve(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ResolveRequest>,
) -> Reply {
    authorize(&state, &headers)?;
    state
        .plans
        .resolve(&id, request.retry)
        .map(view)
        .map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::requested_goal;

    #[test]
    fn plan_requests_need_the_keyword_and_a_separator() {
        assert_eq!(
            requested_goal("Plan: write the time to notes/time.txt"),
            Some("write the time to notes/time.txt")
        );
        assert_eq!(
            requested_goal("  plan, check disk usage."),
            Some("check disk usage.")
        );
        assert_eq!(requested_goal("PLAN - list issues"), Some("list issues"));
        assert_eq!(requested_goal("plan a trip to Lisbon"), None);
        assert_eq!(requested_goal("Planet facts"), None);
        assert_eq!(requested_goal("Plan:"), None);
        assert_eq!(requested_goal("plán: x"), None);
        assert_eq!(requested_goal("pl"), None);
    }
}
