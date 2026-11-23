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
    protocol::mesh::{LendMode, LendRequest, MESH_VERSION, MeshMode, MeshStatus},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// HTTP error response tuple returned by Mesh routes.
pub type Failure = (StatusCode, Json<Value>);

/// Backend operations for `/api/mind/mesh` overview, refresh, and compute lending.
///
/// Extracted so HTTP tests can run against a stub pool over real TCP without
/// constructing a full `MindService`.
#[async_trait::async_trait]
pub trait MeshComputeBackend: Send + Sync {
    /// Returns the JSON body for `GET /api/mind/mesh`.
    async fn overview(&self) -> Value;
    /// Refreshes worker health and returns the updated overview JSON.
    async fn refresh(&self) -> Result<Value, Failure>;
    /// Forwards a [`LendMode`] change to the named worker and returns its updated `MeshWorkerStatus` JSON.
    async fn set_lend(&self, name: &str, mode: LendMode) -> Result<Value, Failure>;
}

struct MindComputeBackend {
    inference: MindService,
}

#[async_trait::async_trait]
impl MeshComputeBackend for MindComputeBackend {
    async fn overview(&self) -> Value {
        let mut value =
            serde_json::to_value(status_from(&self.inference)).unwrap_or_else(|_| json!({}));
        // Claude wires MindService::heavy() at integration.
        if let Some(obj) = value.as_object_mut() {
            obj.entry("heavy").or_insert(Value::Null);
        }
        value
    }

    async fn refresh(&self) -> Result<Value, Failure> {
        pool_from(&self.inference)?.refresh().await;
        Ok(self.overview().await)
    }

    async fn set_lend(&self, name: &str, mode: LendMode) -> Result<Value, Failure> {
        let pool = pool_from(&self.inference)?;
        let worker = pool.set_lend(name, mode).await.map_err(op_error)?;
        Ok(serde_json::to_value(worker).unwrap_or_else(|_| json!({})))
    }
}

struct PoolComputeBackend {
    pool: Arc<sail_mind::mesh::MeshPool>,
}

#[async_trait::async_trait]
impl MeshComputeBackend for PoolComputeBackend {
    async fn overview(&self) -> Value {
        let mut value = serde_json::to_value(self.pool.status()).unwrap_or_else(|_| json!({}));
        // Claude wires MindService::heavy() at integration.
        if let Some(obj) = value.as_object_mut() {
            obj.entry("heavy").or_insert(Value::Null);
        }
        value
    }

    async fn refresh(&self) -> Result<Value, Failure> {
        self.pool.refresh().await;
        Ok(self.overview().await)
    }

    async fn set_lend(&self, name: &str, mode: LendMode) -> Result<Value, Failure> {
        let worker = self.pool.set_lend(name, mode).await.map_err(op_error)?;
        Ok(serde_json::to_value(worker).unwrap_or_else(|_| json!({})))
    }
}

struct HttpState {
    inference: Option<MindService>,
    backend: Arc<dyn MeshComputeBackend>,
    authority: Option<ApprovalAuthority>,
}

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
        MeshOpError::NoCompute(_) => return error(StatusCode::CONFLICT, "no_compute"),
        MeshOpError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, e.to_string())
}

fn pool_from(inference: &MindService) -> Result<&Arc<sail_mind::mesh::MeshPool>, Failure> {
    inference.mesh().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SAIL Mesh is not available in this process",
        )
    })
}

fn pool(s: &HttpState) -> Result<&Arc<sail_mind::mesh::MeshPool>, Failure> {
    let inference = s.inference.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SAIL Mesh is not available in this process",
        )
    })?;
    pool_from(inference)
}

/// Builds the `/api/mind/mesh` router backed by `MindService`.
pub fn router(inference: MindService, authority: Option<ApprovalAuthority>) -> Router {
    let backend: Arc<dyn MeshComputeBackend> = Arc::new(MindComputeBackend {
        inference: inference.clone(),
    });
    build_router(Some(inference), backend, authority)
}

/// Builds the `/api/mind/mesh` router with a custom [`MeshComputeBackend`] (used by tests).
pub fn router_with_backend(
    backend: Arc<dyn MeshComputeBackend>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    build_router(None, backend, authority)
}

/// Builds the `/api/mind/mesh` router backed directly by a [`sail_mind::mesh::MeshPool`] (used by tests).
pub fn router_with_pool(
    pool: Arc<sail_mind::mesh::MeshPool>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    build_router(None, Arc::new(PoolComputeBackend { pool }), authority)
}

fn build_router(
    inference: Option<MindService>,
    backend: Arc<dyn MeshComputeBackend>,
    authority: Option<ApprovalAuthority>,
) -> Router {
    Router::new()
        .route("/", get(overview))
        .route("/refresh", post(refresh))
        .route("/pair", post(pair))
        .route("/workers/:name/enabled", post(enabled))
        .route("/workers/:name/lend", post(lend))
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
            backend,
            authority,
        }))
}

fn status_from(inference: &MindService) -> MeshStatus {
    inference.mesh().map_or(
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

async fn overview(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Result<Json<Value>, Failure> {
    authorize(&s, &h)?;
    Ok(Json(s.backend.overview().await))
}

async fn refresh(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Result<Json<Value>, Failure> {
    authorize(&s, &h)?;
    Ok(Json(s.backend.refresh().await?))
}

async fn lend(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<LendRequest>,
) -> Result<Json<Value>, Failure> {
    authorize(&s, &h)?;
    Ok(Json(s.backend.set_lend(&name, body.mode).await?))
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
