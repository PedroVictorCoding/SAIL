//! Owner-only local Flow editor API. Nodes are pure, bounded text operations;
//! external effects cannot be represented in the v1 contract.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use sail_flow::workflows::{Error, Store};
use sail_guardian::authority::ApprovalAuthority;
use sail_protocol::flow::{RunState, SaveWorkflow, StartRun};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;
struct HttpState {
    store: Option<Arc<Store>>,
    authority: Option<ApprovalAuthority>,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn fail(e: Error) -> (StatusCode, Json<Value>) {
    let status = match e {
        Error::Invalid(_) => StatusCode::BAD_REQUEST,
        Error::Conflict(_) => StatusCode::CONFLICT,
        Error::NotFound => StatusCode::NOT_FOUND,
        Error::Limit => StatusCode::TOO_MANY_REQUESTS,
        Error::Storage => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(json!({"error":e.to_string()})))
}
fn authorize(s: &HttpState, h: &HeaderMap) -> Result<Arc<Store>, (StatusCode, Json<Value>)> {
    let authority = s.authority.as_ref().ok_or_else(|| fail(Error::Storage))?;
    let token = h
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if !token.is_some_and(|t| authority.authenticates(t)) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"owner token required"})),
        ));
    }
    s.store.clone().ok_or_else(|| fail(Error::Storage))
}
/// The daemon's one Flow store, shared by these routes and the scheduler.
fn shared_store() -> Option<Arc<Store>> {
    static STORE: std::sync::OnceLock<Option<Arc<Store>>> = std::sync::OnceLock::new();
    STORE
        .get_or_init(|| {
            match Store::open(sail_common::storage::data_path("flow").join("workflows-v1.json")) {
                Ok(s) => Some(Arc::new(s)),
                Err(e) => {
                    tracing::error!(error=%e,"Flow store unavailable");
                    None
                }
            }
        })
        .clone()
}
pub fn router(authority: Option<ApprovalAuthority>) -> Router {
    router_with_store(shared_store(), authority)
}
/// Run a saved workflow to the end (scheduled runs) and summarise its receipts.
pub async fn run_workflow(id: &str) -> Result<String, String> {
    let store = shared_store().ok_or("Flow is unavailable")?;
    let workflow_id = Uuid::parse_str(id).map_err(|_| format!("no workflow {id}"))?;
    tokio::task::spawn_blocking(move || {
        let (workflows, _) = store.list().map_err(|e| e.to_string())?;
        let workflow = workflows
            .into_iter()
            .find(|w| w.id == workflow_id)
            .ok_or_else(|| "the workflow was deleted".to_string())?;
        let run = store
            .start(StartRun {
                id: Uuid::new_v4(),
                workflow_id,
                revision: workflow.revision,
                input: String::new(),
            })
            .map_err(|e| e.to_string())?;
        // Nodes are pure, bounded text operations; a workflow has at most a few hundred.
        for _ in 0..10_000 {
            if !store.step(run.id).map_err(|e| e.to_string())? {
                break;
            }
        }
        let (_, runs) = store.list().map_err(|e| e.to_string())?;
        let run = runs
            .into_iter()
            .find(|r| r.id == run.id)
            .ok_or_else(|| "the run disappeared".to_string())?;
        let last = run
            .receipts
            .iter()
            .rev()
            .find(|r| !r.skipped)
            .map(|r| r.output.clone())
            .unwrap_or_default();
        match run.state {
            RunState::Completed => Ok(format!("{} finished.\n{last}", workflow.name)),
            state => Err(format!(
                "{} stopped ({state:?}): {}",
                workflow.name,
                run.error.unwrap_or_default()
            )),
        }
    })
    .await
    .map_err(|_| "the workflow worker failed".to_string())?
}
fn router_with_store(store: Option<Arc<Store>>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/workflows", get(list).post(save))
        .route("/status", get(status))
        .route("/runs", post(start))
        .route("/workflows/:id/delete", post(delete_workflow))
        .route("/runs/:id/delete", post(delete_run))
        .route("/runs/:id/cancel", post(cancel))
        .route("/runs/:id/resume", post(resume))
        .layer(axum::extract::DefaultBodyLimit::max(64_000))
        .layer(axum::middleware::map_response(
            |mut r: axum::response::Response| async move {
                r.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                r
            },
        ))
        .with_state(Arc::new(HttpState { store, authority }))
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, (StatusCode, Json<Value>)> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| fail(Error::Storage))?
        .map_err(fail)
}
async fn status(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Reply {
    let store = authorize(&s, &h)?;
    let status = blocking(move || store.status()).await?;
    Ok(Json(json!(status)))
}
async fn list(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Reply {
    let store = authorize(&s, &h)?;
    let (workflows, runs) = blocking(move || store.list()).await?;
    Ok(Json(json!({"version":1,"workflows":workflows,"runs":runs})))
}
async fn save(State(s): State<Arc<HttpState>>, h: HeaderMap, Json(q): Json<SaveWorkflow>) -> Reply {
    let store = authorize(&s, &h)?;
    let workflow = blocking(move || store.save(q)).await?;
    Ok(Json(json!({"workflow":workflow})))
}
fn drive(store: Arc<Store>, id: Uuid) {
    tokio::spawn(async move {
        loop {
            let worker = store.clone();
            match tokio::task::spawn_blocking(move || worker.step(id)).await {
                Ok(Ok(true)) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
                Ok(Ok(false)) | Ok(Err(Error::NotFound)) => break,
                _ => {
                    tracing::error!(%id,"Flow checkpoint failed; execution stopped");
                    break;
                }
            }
        }
    });
}
async fn start(State(s): State<Arc<HttpState>>, h: HeaderMap, Json(q): Json<StartRun>) -> Reply {
    let store = authorize(&s, &h)?;
    let worker = store.clone();
    let run = blocking(move || worker.start(q)).await?;
    if run.state == RunState::Running {
        drive(store, run.id);
    }
    Ok(Json(json!({"run":run})))
}
async fn cancel(State(s): State<Arc<HttpState>>, h: HeaderMap, Path(id): Path<Uuid>) -> Reply {
    let store = authorize(&s, &h)?;
    let run = blocking(move || store.control(id, false)).await?;
    Ok(Json(json!({"run":run})))
}
async fn resume(State(s): State<Arc<HttpState>>, h: HeaderMap, Path(id): Path<Uuid>) -> Reply {
    let store = authorize(&s, &h)?;
    let worker = store.clone();
    let run = blocking(move || worker.control(id, true)).await?;
    drive(store, id);
    Ok(Json(json!({"run":run})))
}
async fn delete_workflow(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Reply {
    let store = authorize(&s, &h)?;
    blocking(move || store.delete_workflow(id)).await?;
    Ok(Json(json!({"deleted": id})))
}
async fn delete_run(State(s): State<Arc<HttpState>>, h: HeaderMap, Path(id): Path<Uuid>) -> Reply {
    let store = authorize(&s, &h)?;
    blocking(move || store.delete_run(id)).await?;
    Ok(Json(json!({"deleted": id})))
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test]
    async fn owner_required_even_for_reads() {
        let app = router_with_store(
            None,
            Some(ApprovalAuthority::new("flow-test-owner-secret-01234567890").unwrap()),
        );
        let r = app
            .oneshot(
                Request::builder()
                    .uri("/workflows")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn real_api_saves_executes_and_deduplicates() {
        use axum::body::to_bytes;
        let path = std::env::temp_dir()
            .join(format!("flow-http-{}", Uuid::new_v4()))
            .join("state.json");
        let store = Arc::new(Store::open(path.clone()).unwrap());
        let token = "flow-test-owner-secret-01234567890";
        let app = router_with_store(Some(store), Some(ApprovalAuthority::new(token).unwrap()));
        async fn request(
            app: &Router,
            path: &str,
            body: Option<Value>,
            token: &str,
        ) -> (StatusCode, Value) {
            let req = Request::builder()
                .method(if body.is_some() { "POST" } else { "GET" })
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            (status, serde_json::from_slice(&body).unwrap())
        }
        assert_eq!(
            request(&app, "/workflows", None, "wrong").await.0,
            StatusCode::UNAUTHORIZED
        );
        let workflow = Uuid::new_v4();
        let id = Uuid::new_v4();
        let body = json!({"expected_revision":0,"workflow":{"version":1,"id":workflow,"revision":1,"name":"HTTP workflow","nodes":[
            {"id":"start","source":null,"operation":{"kind":"trigger"}},
            {"id":"upper","source":"start","operation":{"kind":"uppercase"}},
            {"id":"output","source":"upper","operation":{"kind":"output"}}
        ]}});
        assert_eq!(
            request(&app, "/workflows", Some(body.clone()), token)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            request(&app, "/workflows", Some(body), token).await.0,
            StatusCode::CONFLICT
        );
        let run = json!({"id":id,"workflow_id":workflow,"revision":1,"input":"hello"});
        assert_eq!(
            request(&app, "/runs", Some(run.clone()), token).await.0,
            StatusCode::OK
        );
        for _ in 0..100 {
            let (_, data) = request(&app, "/workflows", None, token).await;
            if data["runs"][0]["state"] == "completed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (_, data) = request(&app, "/workflows", None, token).await;
        assert_eq!(data["runs"][0]["state"], "completed");
        let (_, status) = request(&app, "/status", None, token).await;
        assert_eq!(status["workflows"], 1);
        assert_eq!(status["running"], 0);
        assert!(status.get("runs").is_none());
        assert_eq!(data["runs"][0]["receipts"][2]["output"], "HELLO");
        assert_eq!(
            request(&app, "/runs", Some(run), token).await.0,
            StatusCode::OK
        );
        assert_eq!(
            request(&app, "/workflows", None, token).await.1["runs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
