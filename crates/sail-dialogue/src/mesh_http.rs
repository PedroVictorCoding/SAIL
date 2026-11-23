//! Owner-only SAIL Mesh API, `/api/mind/mesh`: list, pair, switch and forget worker PCs
//! that share the host's inference. Responses never contain a worker's secret.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use sail_guardian::authority::ApprovalAuthority;
use sail_mind::{
    MindService,
    mesh::{MeshOpError, pair_at, remove_at, set_enabled_at},
    protocol::mesh::{MESH_VERSION, MeshMode, MeshStatus},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct HttpState {
    inference: MindService,
    authority: Option<ApprovalAuthority>,
}
type Failure = (StatusCode, Json<Value>);

fn error(status: StatusCode, message: impl Into<String>) -> Failure {
    (status, Json(json!({"error": message.into()})))
}

fn authorize(s: &HttpState, h: &HeaderMap) -> Result<(), Failure> {
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

fn op_error(e: MeshOpError) -> Failure {
    let status = match &e {
        MeshOpError::BadInput(_) => StatusCode::BAD_REQUEST,
        MeshOpError::NotFound(_) => StatusCode::NOT_FOUND,
        MeshOpError::Unverified(_) => StatusCode::BAD_GATEWAY,
        MeshOpError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, e.to_string())
}

fn pool(s: &HttpState) -> Result<&Arc<sail_mind::mesh::MeshPool>, Failure> {
    s.inference.mesh().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SAIL Mesh is not available in this process",
        )
    })
}

pub fn router(inference: MindService, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/", get(overview))
        .route("/refresh", post(refresh))
        .route("/pair", post(pair))
        .route("/workers/:name/enabled", post(enabled))
        .route("/workers/:name", delete(remove))
        .layer(axum::extract::DefaultBodyLimit::max(8_000))
        .layer(axum::middleware::map_response(
            |mut r: axum::response::Response| async move {
                r.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                r
            },
        ))
        .with_state(Arc::new(HttpState {
            inference,
            authority,
        }))
}

fn status(s: &HttpState) -> MeshStatus {
    s.inference.mesh().map_or(
        MeshStatus {
            version: MESH_VERSION,
            mode: MeshMode::Off,
            workers: vec![],
            offloaded: 0,
            fell_back: 0,
            heavy: None,
        },
        |pool| pool.status(),
    )
}

async fn overview(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
) -> Result<Json<MeshStatus>, Failure> {
    authorize(&s, &h)?;
    Ok(Json(status(&s)))
}

async fn refresh(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
) -> Result<Json<MeshStatus>, Failure> {
    authorize(&s, &h)?;
    pool(&s)?.refresh().await;
    Ok(Json(status(&s)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairBody {
    code: String,
    #[serde(default)]
    serves: Vec<String>,
}

async fn pair(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Json(body): Json<PairBody>,
) -> Result<Json<Value>, Failure> {
    authorize(&s, &h)?;
    let pool = pool(&s)?;
    let (record, _hello, replaced) = pair_at(pool.registry_path(), &body.code, body.serves)
        .await
        .map_err(op_error)?;
    let worker = pool.refreshed_worker(&record.name).await;
    Ok(Json(json!({"worker": worker, "replaced": replaced})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnabledBody {
    enabled: bool,
}

async fn enabled(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<EnabledBody>,
) -> Result<Json<Value>, Failure> {
    authorize(&s, &h)?;
    let pool = pool(&s)?;
    set_enabled_at(pool.registry_path(), &name, body.enabled).map_err(op_error)?;
    let worker = pool.refreshed_worker(&name).await;
    Ok(Json(json!({"worker": worker})))
}

async fn remove(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, Failure> {
    authorize(&s, &h)?;
    let pool = pool(&s)?;
    remove_at(pool.registry_path(), &name).map_err(op_error)?;
    pool.refresh().await;
    Ok(StatusCode::NO_CONTENT)
}
