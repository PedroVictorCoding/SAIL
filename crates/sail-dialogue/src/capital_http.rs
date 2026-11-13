//! SAIL Capital owner API, mounted at `/api/capital`.
//!
//! Every route requires the owner's bearer token. All routes are read-only except
//! `POST /import/xtb`, which books a report file the owner exported and uploaded. Nothing
//! here contacts a broker, and amounts are serialized as decimal strings. See
//! `docs/SAIL_CAPITAL.md`.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use sail_capital::AsOf;
use sail_capital::service::{CapitalService, GrantInput, ServiceError};
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
        .route("/filings", get(filings))
        .route("/sec/status", get(sec_status))
        .route("/sec/refresh", post(sec_refresh))
        .route("/insights", get(insights))
        .route("/grants", get(grants).post(record_grant))
        .route("/grants/:id/retract", post(retract_grant))
        .route("/prices/status", get(price_status))
        .route("/prices/refresh", post(price_refresh))
        .route("/prices/import", post(price_import))
        .route("/prices", get(prices))
        .route("/forecasts", get(forecasts))
        .route("/forecasts/evaluation", get(forecast_evaluation))
        .route("/today", get(today))
        .route("/suggestions/:id/action", post(suggestion_action))
        .route("/policy", get(policy).post(save_policy))
        .route("/signals", get(signals))
        .route("/signals/board", get(signal_board))
        .route("/signals/board/record", get(board_record))
        .route("/signals/models", get(signal_models))
        .route("/signals/refresh", post(signal_refresh))
        .route("/outlook", get(outlook))
        .route("/signals/trade", get(trade_view))
        .route("/signals/trade/search", get(trade_search))
        .route("/signals/trade/check", get(trade_check))
        .route("/signals/trade/run", post(trade_run))
        .route("/signals/trade/record", get(trade_record))
        .route("/signals/trade/guards", get(trade_guards))
        .route("/signals/trade/health", get(trade_health))
        .route("/universe/watchlist", get(watchlist).post(change_watchlist))
        .route("/dividends/projection", get(dividend_projection))
        .route("/dividends/refresh", post(dividend_refresh))
        .route("/outlook/track-record", get(track_record))
        .route("/scorecard", get(scorecard))
        .route("/markets", get(markets))
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
    currency: Option<String>,
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

async fn insights(
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
    let insights = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.insights(&account, as_of, now)
    })
    .await?;
    Ok(no_store(to_json(&insights)?))
}

async fn grants(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let grants = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.grants(&account)
    })
    .await?;
    Ok(no_store(to_json(&grants)?))
}

async fn record_grant(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
    body: Bytes,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let input: GrantInput = serde_json::from_slice(&body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "The grant must be JSON with symbol, quantity and received_at."})),
        )
    })?;
    let grant = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.record_grant(&account, input, Utc::now())
    })
    .await?;
    Ok(no_store(to_json(&grant)?))
}

async fn retract_grant(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let grant = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.retract_grant(&account, &id, Utc::now())
    })
    .await?;
    Ok(no_store(to_json(&grant)?))
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
            q.currency.as_deref(),
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

async fn filings(
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
    let view = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.filings(&account, as_of)
    })
    .await?;
    Ok(no_store(to_json(&view)?))
}

async fn sec_status(State(s): State<Arc<CapitalState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    Ok(no_store(to_json(&s.service.sec_status())?))
}

/// Runs one SEC refresh now. Only SEC is contacted, and only with CIKs of held issuers.
async fn price_status(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let status = blocking(&s, |svc| svc.price_status()).await?;
    Ok(no_store(to_json(&status)?))
}

async fn price_refresh(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let account = match q.account.as_deref().filter(|a| !a.is_empty()) {
        Some(id) => {
            let id = id.to_string();
            Some(blocking(&s, move |svc| svc.resolve_account(Some(&id))).await?)
        }
        None => None,
    };
    let status = s
        .service
        .refresh_prices(account.as_ref())
        .await
        .map_err(failure)?;
    Ok(no_store(to_json(&status)?))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PriceImportQuery {
    account: Option<String>,
    symbol: String,
    currency: Option<String>,
    adjustment: Option<String>,
}

async fn price_import(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<PriceImportQuery>,
    body: Bytes,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let adjustment = match q.adjustment.as_deref().unwrap_or("unknown") {
        "unadjusted" => sail_capital::market::Adjustment::Unadjusted,
        "split_adjusted" => sail_capital::market::Adjustment::SplitAdjusted,
        "total_return" => sail_capital::market::Adjustment::TotalReturn,
        "unknown" => sail_capital::market::Adjustment::Unknown,
        _ => {
            return Err(failure(ServiceError::BadRequest(
                "adjustment must be unadjusted, split_adjusted, total_return or unknown.".into(),
            )));
        }
    };
    let result = blocking(&s, move |svc| {
        let account = if q.account.as_deref().is_some_and(|a| !a.is_empty()) {
            Some(svc.resolve_account(q.account.as_deref())?)
        } else {
            svc.resolve_account(None).ok()
        };
        svc.import_price_file(
            account.as_ref(),
            &q.symbol,
            &body,
            q.currency.as_deref(),
            adjustment,
            Utc::now(),
        )
    })
    .await?;
    Ok(no_store(to_json(&result)?))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PricesQuery {
    symbol: String,
    from: Option<chrono::NaiveDate>,
    to: Option<chrono::NaiveDate>,
}

async fn prices(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<PricesQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let rows = blocking(&s, move |svc| svc.prices(&q.symbol, q.from, q.to)).await?;
    Ok(no_store(to_json(&rows)?))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ForecastQuery {
    account: Option<String>,
    horizon: Option<u32>,
    model: Option<sail_capital::forecast::ModelId>,
}

async fn forecasts(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<ForecastQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let view = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.forecasts(
            &account,
            q.horizon.unwrap_or(20),
            q.model.unwrap_or(sail_capital::forecast::ModelId::NoChange),
        )
    })
    .await?;
    Ok(no_store(to_json(&view)?))
}

async fn forecast_evaluation(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let report = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.evaluation(&account)
    })
    .await?;
    Ok(no_store(to_json(&report)?))
}

async fn sec_refresh(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let account = match q.account.as_deref().filter(|a| !a.is_empty()) {
        Some(id) => {
            let id = id.to_string();
            Some(blocking(&s, move |svc| svc.resolve_account(Some(&id))).await?)
        }
        None => None,
    };
    let status = s.service.refresh_sec(account.as_ref()).await;
    Ok(no_store(to_json(&status)?))
}

async fn today(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let today = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.today(&account, Utc::now())
    })
    .await?;
    Ok(no_store(to_json(&today)?))
}

#[derive(Deserialize)]
struct ActionBody {
    action: sail_capital::suggest::Action,
}

async fn suggestion_action(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<AccountQuery>,
    body: Bytes,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let input: ActionBody = serde_json::from_slice(&body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "The body must be {\"action\": \"dismissed\"} or {\"action\": \"done\"}."})),
        )
    })?;
    let record = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.suggestion_action(&account, &id, input.action, Utc::now())
    })
    .await?;
    Ok(no_store(to_json(&record)?))
}

async fn policy(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let view = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.policy(&account)
    })
    .await?;
    Ok(no_store(to_json(&view)?))
}

#[derive(Deserialize)]
struct PolicyBody {
    limits: sail_capital::policy::RiskLimits,
    #[serde(default)]
    confirm: bool,
}

async fn save_policy(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
    body: Bytes,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let input: PolicyBody = serde_json::from_slice(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("The limits couldn't be read: {e}")})),
        )
    })?;
    let saved = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.save_policy(&account, &input.limits, input.confirm, Utc::now())
    })
    .await?;
    Ok(no_store(to_json(&saved)?))
}

async fn signals(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let report = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.signals(&account)
    })
    .await?;
    Ok(no_store(to_json(&report)?))
}

async fn signal_board(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let board = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.signal_board(&account)
    })
    .await?;
    Ok(no_store(to_json(&board)?))
}

async fn board_record(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let record = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.board_record(&account)
    })
    .await?;
    Ok(no_store(to_json(&record)?))
}

async fn markets(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let board = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.markets(&account)
    })
    .await?;
    Ok(no_store(to_json(&board)?))
}

async fn scorecard(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let card = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.scorecard(&account)
    })
    .await?;
    Ok(no_store(to_json(&card)?))
}

async fn signal_models(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let status = blocking(&s, move |svc| svc.models_status()).await?;
    Ok(no_store(to_json(&status)?))
}

async fn signal_refresh(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let id = q.account.clone();
    let account = blocking(&s, move |svc| svc.resolve_account(id.as_deref())).await?;
    // A first run can take many minutes; it runs in the background and the page polls
    // `/signals/models`.
    let service = s.service.clone();
    tokio::spawn(async move {
        if let Err(e) = service.refresh_model_signals(&account).await {
            tracing::warn!(error = %e, "model signal refresh failed");
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let status = blocking(&s, move |svc| svc.models_status()).await?;
    Ok(no_store(to_json(&status)?))
}

#[derive(Deserialize)]
struct OutlookQuery {
    account: Option<String>,
    symbol: String,
    horizon: Option<u32>,
    cutoff: Option<chrono::NaiveDate>,
}

async fn outlook(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<OutlookQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let view = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.outlook(&account, &q.symbol, q.horizon.unwrap_or(63), q.cutoff)
    })
    .await?;
    Ok(no_store(to_json(&view)?))
}

#[derive(Deserialize)]
struct TrackQuery {
    account: Option<String>,
    horizon: Option<u32>,
}

async fn track_record(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<TrackQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let record = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.track_record(&account, q.horizon.unwrap_or(63))
    })
    .await?;
    Ok(no_store(to_json(&record)?))
}

#[derive(Deserialize)]
struct TradeSearchQuery {
    q: String,
}

async fn trade_search(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<TradeSearchQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let results = s
        .service
        .search_trade_symbols(&q.q)
        .await
        .map_err(failure)?;
    Ok(no_store(to_json(&results)?))
}

#[derive(Deserialize)]
struct TradeCheckQuery {
    account: Option<String>,
    symbol: String,
    #[serde(default)]
    yahoo: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    exchange: Option<String>,
    #[serde(default)]
    class: Option<String>,
}

async fn trade_check(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<TradeCheckQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let id = q.account.clone();
    let account = blocking(&s, move |svc| svc.resolve_account(id.as_deref())).await?;
    let check = s
        .service
        .check_trade_signal(
            &account,
            &q.symbol,
            q.yahoo.as_deref(),
            q.name.as_deref(),
            q.exchange.as_deref(),
            q.class.as_deref(),
        )
        .await
        .map_err(failure)?;
    Ok(no_store(to_json(&check)?))
}

async fn trade_view(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let view = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.trade_view(&account)
    })
    .await?;
    Ok(no_store(to_json(&view)?))
}

async fn trade_run(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let id = q.account.clone();
    let account = blocking(&s, move |svc| svc.resolve_account(id.as_deref())).await?;
    // A run takes minutes, longer than a tunnel keeps the request open, so it runs in
    // the background and the page polls GET /signals/trade while `running`.
    s.service.start_trade_run(account.clone());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let view = blocking(&s, move |svc| svc.trade_view(&account)).await?;
    Ok(no_store(to_json(&view)?))
}

async fn trade_record(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let record = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.trade_record(&account)
    })
    .await?;
    Ok(no_store(to_json(&record)?))
}

async fn trade_guards(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let guards = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.trade_guards(&account)
    })
    .await?;
    Ok(no_store(to_json(&guards)?))
}

async fn trade_health(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let health = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.trade_health(&account)
    })
    .await?;
    Ok(no_store(to_json(&health)?))
}

async fn watchlist(State(s): State<Arc<CapitalState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let list = blocking(&s, move |svc| svc.watchlist()).await?;
    Ok(no_store(to_json(&list)?))
}

async fn change_watchlist(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    body: Bytes,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let change: sail_capital::universe::WatchChange =
        serde_json::from_slice(&body).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("The change couldn't be read: {e}")})),
            )
        })?;
    let list = blocking(&s, move |svc| svc.change_watchlist(change, Utc::now())).await?;
    Ok(no_store(to_json(&list)?))
}

#[derive(Deserialize)]
struct ProjectionQuery {
    account: Option<String>,
    years: Option<u32>,
    reinvest_pct: Option<f64>,
    allocation: Option<sail_capital::dividends::Allocation>,
    top_n: Option<u32>,
    reserve_pt_tax: Option<bool>,
}

async fn dividend_projection(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<ProjectionQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let outlook = blocking(&s, move |svc| {
        let account = svc.resolve_account(q.account.as_deref())?;
        svc.dividend_projection_with(
            &account,
            q.years.unwrap_or(10),
            q.reinvest_pct.unwrap_or(100.0),
            q.allocation
                .unwrap_or(sail_capital::dividends::Allocation::Proportional),
            q.top_n.unwrap_or(sail_capital::dividends::DEFAULT_TOP_N),
            q.reserve_pt_tax.unwrap_or(false),
        )
    })
    .await?;
    Ok(no_store(to_json(&outlook)?))
}

async fn dividend_refresh(
    State(s): State<Arc<CapitalState>>,
    h: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let id = q.account.clone();
    let account = blocking(&s, move |svc| svc.resolve_account(id.as_deref())).await?;
    let result = s
        .service
        .refresh_dividend_histories(&account)
        .await
        .map_err(failure)?;
    Ok(no_store(to_json(&result)?))
}

#[cfg(test)]
mod tests {
    use super::ProjectionQuery;
    use sail_capital::dividends::Allocation;

    #[test]
    fn projection_query_parses_the_new_allocations_and_top_n() {
        let query: ProjectionQuery =
            serde_json::from_value(serde_json::json!({"allocation": "top_value", "top_n": 7}))
                .unwrap();
        assert_eq!(query.allocation, Some(Allocation::TopValue));
        assert_eq!(query.top_n, Some(7));

        let query: ProjectionQuery =
            serde_json::from_value(serde_json::json!({"allocation": "underweight"})).unwrap();
        assert_eq!(query.allocation, Some(Allocation::Underweight));
        assert_eq!(query.top_n, None);
    }
}
