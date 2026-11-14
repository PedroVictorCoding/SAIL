//! Owner-only SAIL Market API, nested at `/api/hermes/market` (round 20). Browsing,
//! staging and installing Hermes skills and plugins; see `sail_flow::market` for the flow.
//! Installing never runs anything: an installed plugin is still quarantined until the
//! owner approves its exact files on the Skills & plugins section.
use axum::{
    Json, Router,
    extract::{Query as QueryParams, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use sail_flow::market::{Kind, Market, Query, Trust};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct MarketState {
    market: Arc<Market>,
    authority: Option<ApprovalAuthority>,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message.into()})))
}

fn authorize(s: &MarketState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
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

pub fn router(market: Arc<Market>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/", get(search))
        .route("/refresh", post(refresh))
        .route("/stage", post(stage))
        .route("/install", post(install))
        .route("/discard", post(discard))
        .route("/installed", get(installed))
        .route("/uninstall", post(uninstall))
        .with_state(Arc::new(MarketState { market, authority }))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchParams {
    text: String,
    kind: Option<Kind>,
    source: Option<String>,
    min_trust: Option<Trust>,
    offset: usize,
    limit: usize,
}

async fn search(
    State(s): State<Arc<MarketState>>,
    h: HeaderMap,
    QueryParams(p): QueryParams<SearchParams>,
) -> Reply {
    authorize(&s, &h)?;
    if p.text.len() > 200 {
        return Err(error(StatusCode::BAD_REQUEST, "search text is too long"));
    }
    s.market.refresh_if_stale();
    let page = s
        .market
        .search(Query {
            text: p.text,
            kind: p.kind,
            source: p.source.filter(|v| !v.is_empty()),
            min_trust: p.min_trust,
            offset: p.offset,
            limit: p.limit,
        })
        .await
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(
        json!({"status": s.market.status().await, "page": page}),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RefreshBody {
    force: bool,
}

async fn refresh(
    State(s): State<Arc<MarketState>>,
    h: HeaderMap,
    body: Option<Json<RefreshBody>>,
) -> Reply {
    authorize(&s, &h)?;
    let force = body.map(|b| b.force).unwrap_or(false);
    match s.market.refresh(force).await {
        Ok(status) => Ok(Json(json!({"status": status}))),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": e, "status": s.market.status().await})),
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StageBody {
    kind: Kind,
    id: String,
}

async fn stage(State(s): State<Arc<MarketState>>, h: HeaderMap, Json(b): Json<StageBody>) -> Reply {
    authorize(&s, &h)?;
    if b.id.is_empty() || b.id.len() > 300 {
        return Err(error(StatusCode::BAD_REQUEST, "invalid id"));
    }
    s.market
        .item(b.kind, &b.id)
        .await
        .map_err(|e| error(StatusCode::NOT_FOUND, e))?;
    let report = s
        .market
        .stage(b.kind, &b.id)
        .await
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e))?;
    tracing::info!(id = %b.id, stage = %report.stage_id, verdict = ?report.report.verdict, "Market item staged by owner");
    Ok(Json(json!(report)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallBody {
    stage_id: String,
    #[serde(default)]
    acknowledge_caution: bool,
}

fn valid_stage_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

async fn install(
    State(s): State<Arc<MarketState>>,
    h: HeaderMap,
    Json(b): Json<InstallBody>,
) -> Reply {
    authorize(&s, &h)?;
    if !valid_stage_id(&b.stage_id) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid stage id"));
    }
    let entry = s
        .market
        .install(&b.stage_id, b.acknowledge_caution)
        .await
        .map_err(|e| error(StatusCode::CONFLICT, e))?;
    tracing::info!(name = %entry.name, kind = ?entry.kind, hash = %entry.content_hash, "Market item installed by owner");
    Ok(Json(json!({"entry": entry})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscardBody {
    stage_id: String,
}

async fn discard(
    State(s): State<Arc<MarketState>>,
    h: HeaderMap,
    Json(b): Json<DiscardBody>,
) -> Reply {
    authorize(&s, &h)?;
    if !valid_stage_id(&b.stage_id) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid stage id"));
    }
    s.market
        .discard(&b.stage_id)
        .await
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"ok": true})))
}

async fn installed(State(s): State<Arc<MarketState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let list = s
        .market
        .installed()
        .await
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"installed": list})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UninstallBody {
    key: String,
    #[serde(default)]
    force: bool,
}

async fn uninstall(
    State(s): State<Arc<MarketState>>,
    h: HeaderMap,
    Json(b): Json<UninstallBody>,
) -> Reply {
    authorize(&s, &h)?;
    s.market
        .uninstall(&b.key, b.force)
        .await
        .map_err(|e| error(StatusCode::CONFLICT, e))?;
    tracing::info!(key = %b.key, "Market item uninstalled by owner");
    Ok(Json(json!({"ok": true})))
}
