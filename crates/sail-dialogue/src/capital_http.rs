//! SAIL Capital owner API, mounted at `/api/capital`.
//!
//! Every route requires the owner's bearer token. All routes are read-only except
//! `POST /import/xtb`, which books a report file the owner exported and uploaded. Nothing
//! here contacts a broker, and amounts are serialized as decimal strings. See
//! `docs/SAIL_CAPITAL.md`.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use sail_capital::AsOf;
use sail_capital::service::{CapitalService, ServiceError};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Largest upload accepted; this matches the importer's own cap.
const MAX_UPLOAD_BYTES: usize = sail_capital::MAX_REPORT_BYTES;

struct CapitalState {
    service: Arc<CapitalService>,
    authority: Option<ApprovalAuthority>,
}

type Failure = (StatusCode, Json<Value>);

/// Builds the router. `authority` is the owner-token check; without one, every route
/// answers 503.
pub fn router(service: Arc<CapitalService>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/accounts", get(accounts))
        .route("/portfolio", get(portfolio))
        .route("/positions", get(positions))
        .route("/cashflows", get(cashflows))
        .route("/dividends", get(dividends))
        .route("/sync/status", get(sync_status))
        .route("/conflicts", get(conflicts))
        .route("/import/xtb", post(import_xtb))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 1))
        .with_state(Arc::new(CapitalState { service, authority }))
}

fn authorize(s: &CapitalState, h: &HeaderMap) -> Result<(), Failure> {
    let deny = |code| {
        (
            code,
            Json(json!({"error": "Owner authentication required"})),
        )
    };
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

fn failure(e: ServiceError) -> Failure {
    let (code, message) = match &e {
        ServiceError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
        ServiceError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
        ServiceError::Import(i) => (StatusCode::UNPROCESSABLE_ENTITY, i.to_string()),
        ServiceError::Store(err) => {
            tracing::warn!(error = %err, "Capital storage error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "The Capital ledger is unavailable.".to_string(),
            )
        }
    };
    (code, Json(json!({ "error": message })))
}

fn no_store(value: Value) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

/// Runs a blocking service call off the async runtime.
async fn blocking<T, F>(s: &Arc<CapitalState>, f: F) -> Result<T, Failure>
where
    T: Send + 'static,
    F: FnOnce(&CapitalService) -> Result<T, ServiceError> + Send + 'static,
{
    let service = s.service.clone();
    tokio::task::spawn_blocking(move || f(&service))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "The Capital request failed."})),
            )
        })?
        .map_err(failure)
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "The Capital reply could not be encoded."})),
        )
    })
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AccountQuery {
    account: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PortfolioQuery {
    account: Option<String>,
    occurred_by: Option<DateTime<Utc>>,
    known_by: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RangeQuery {
    account: Option<String>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ImportQuery {
    account: Option<String>,
    tz: Option<String>,
}

async fn accounts(State(s): State<Arc<CapitalState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let list = blocking(&s, |svc| svc.accounts()).await?;
    Ok(no_store(to_json(&list)?))
}

async fn portfolio(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<PortfolioQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let now = Utc::now();
    let as_of = AsOf {
        occurred_by: q.occurred_by.unwrap_or(now).min(now),
        known_by: q.known_by.unwrap_or(now).min(now),
    };
    let projection = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.portfolio(&account, as_of)
    })
    .await?;
    Ok(no_store(to_json(&projection)?))
}

async fn positions(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let p = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.portfolio(&account, AsOf::now())
    })
    .await?;
    Ok(no_store(json!({
        "account": to_json(&p.account)?,
        "positions": to_json(&p.positions)?,
        "cfd_positions": to_json(&p.cfd_positions)?,
        "snapshot_as_of": p.snapshot_as_of,
    })))
}

async fn cashflows(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<RangeQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let list = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.cashflows(&account, q.from, q.to)
    })
    .await?;
    Ok(no_store(to_json(&list)?))
}

async fn dividends(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<RangeQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let list = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.dividends(&account, q.from, q.to)
    })
    .await?;
    Ok(no_store(to_json(&list)?))
}

async fn sync_status(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let list = blocking(&s, move |svc| match q.account.as_deref() {
        Some(id) if !id.is_empty() => {
            let account = svc.resolve_account(Some(id))?;
            svc.batches(Some(&account))
        }
        _ => svc.batches(None),
    })
    .await?;
    Ok(no_store(to_json(&list)?))
}

async fn conflicts(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let list = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.conflicts(&account)
    })
    .await?;
    Ok(no_store(to_json(&list)?))
}

async fn import_xtb(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    if body.len() > MAX_UPLOAD_BYTES {
        return Err(failure(ServiceError::Import(
            sail_capital::ImportError::TooLarge(body.len()),
        )));
    }
    let file_name = h
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.rsplit(['/', '\\']).next().unwrap_or(v).to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            failure(ServiceError::BadRequest(
                "Missing the X-File-Name header (.xlsx or .csv).".into(),
            ))
        })?;
    let status = blocking(&s, move |svc| {
        svc.import_xtb(
            q.account.as_deref(),
            &file_name,
            &body,
            q.tz.as_deref(),
            Utc::now(),
        )
    })
    .await?;
    tracing::info!(
        account = %status.account,
        added = status.added,
        duplicates = status.duplicates,
        conflicts = status.conflicts,
        already = status.already_imported,
        "Capital report imported"
    );
    Ok(no_store(to_json(&status)?))
}
