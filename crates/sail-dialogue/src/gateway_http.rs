//! Owner-only Telegram gateway API, `/api/agents/gateway` (round 22). The Messaging
//! section of SAIL Agents uses it; see [`crate::gateway_service`].
use crate::gateway_service::GatewayService;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::Utc;
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct HttpState {
    service: Arc<GatewayService>,
    authority: Option<ApprovalAuthority>,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message.into()})))
}

fn authorize(s: &HttpState, h: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
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

pub fn router(service: Arc<GatewayService>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/", get(overview))
        .route("/connect", post(connect))
        .route("/disconnect", post(disconnect))
        .route("/pair/approve", post(pair_approve))
        .route("/pair/deny", post(pair_deny))
        .route("/pair/revoke", post(pair_revoke))
        .route("/policy", post(policy))
        .route("/chats/:chat_id/new", post(new_chat))
        .route("/outbox/retry", post(retry))
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
        .with_state(Arc::new(HttpState { service, authority }))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
    status: StatusCode,
) -> Result<T, (StatusCode, Json<Value>)> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "gateway failed"))?
        .map_err(|e| error(status, e))
}

async fn overview(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    let service = s.service.clone();
    let body = blocking(
        move || {
            Ok(json!({
                "status": service.status(),
                "pairing": service.pairing_view()?,
                "chats": service.chats(),
                "outbox": service.outbox_view(),
            }))
        },
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await?;
    Ok(Json(body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectBody {
    token: String,
}

async fn connect(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Json(b): Json<ConnectBody>,
) -> Reply {
    authorize(&s, &h)?;
    if sail_flow::gateway::BotToken::parse(b.token.trim()).is_err() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "that doesn't look like a bot token from @BotFather",
        ));
    }
    let status = s
        .service
        .connect(&b.token)
        .await
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({"status": status})))
}

async fn disconnect(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    Ok(Json(json!({"status": s.service.disconnect()})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBody {
    code: String,
}

async fn pair_approve(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Json(b): Json<CodeBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&s, &h)?;
    let service = s.service.clone();
    let now = Utc::now().timestamp();
    let result = tokio::task::spawn_blocking(move || {
        let paired = service.pairing.approve(&b.code, now);
        let locked = service
            .pairing_view()
            .ok()
            .and_then(|v| v.locked_until)
            .filter(|t| *t > now);
        (paired, locked)
    })
    .await
    .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "pairing failed"))?;
    match result {
        (Ok(paired), _) => {
            tracing::info!(sender = paired.sender_id, "Telegram sender paired by owner");
            s.service.broadcast_one(paired.chat_id);
            Ok(Json(json!({"paired": paired})))
        }
        (Err(e), Some(until)) => Err((
            StatusCode::LOCKED,
            Json(json!({"error": e, "locked_until": until})),
        )),
        (Err(e), None) => Err(error(StatusCode::BAD_REQUEST, e)),
    }
}

async fn pair_deny(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Json(b): Json<CodeBody>,
) -> Reply {
    authorize(&s, &h)?;
    let service = s.service.clone();
    blocking(move || service.pairing.deny(&b.code), StatusCode::BAD_REQUEST).await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeBody {
    sender_id: i64,
}

async fn pair_revoke(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Json(b): Json<RevokeBody>,
) -> Reply {
    authorize(&s, &h)?;
    let service = s.service.clone();
    blocking(move || service.pairing.revoke(b.sender_id), StatusCode::NOT_FOUND).await?;
    tracing::info!(sender = b.sender_id, "Telegram sender revoked by owner");
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyBody {
    policy: String,
}

async fn policy(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Json(b): Json<PolicyBody>,
) -> Reply {
    authorize(&s, &h)?;
    let service = s.service.clone();
    blocking(move || service.pairing.set_policy(&b.policy), StatusCode::BAD_REQUEST).await?;
    Ok(Json(json!({"ok": true})))
}

async fn new_chat(
    State(s): State<Arc<HttpState>>,
    h: HeaderMap,
    Path(chat_id): Path<i64>,
) -> Reply {
    authorize(&s, &h)?;
    if !s.service.new_conversation(chat_id) {
        return Err(error(StatusCode::NOT_FOUND, "no such chat"));
    }
    Ok(Json(json!({"ok": true})))
}

async fn retry(State(s): State<Arc<HttpState>>, h: HeaderMap) -> Reply {
    authorize(&s, &h)?;
    s.service.deliver_once().await;
    Ok(Json(json!({"ok": true})))
}
