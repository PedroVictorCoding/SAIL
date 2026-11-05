//! HTTP router for the Return Briefing & Session Staging engine, nested at `/api/briefing`.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use sail_guardian::authority::ApprovalAuthority;

use crate::briefing::BriefingEngine;

struct HttpState {
    engine: Arc<BriefingEngine>,
    authority: Option<ApprovalAuthority>,
}

pub fn router(engine: Arc<BriefingEngine>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/latest", get(latest))
        .route("/refresh", post(refresh))
        .route("/actions/play-music", post(play_music))
        .route("/actions/dismiss", post(dismiss))
        .layer(axum::extract::DefaultBodyLimit::max(16_384))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(HttpState { engine, authority }))
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn authorize(state: &HttpState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let refuse = |status| (status, Json(json!({"error": "owner token required"})));
    let authority = match &state.authority {
        Some(a) => a,
        None => return Ok(()), // Local development without token
    };
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

async fn latest(State(state): State<Arc<HttpState>>) -> Reply {
    let briefing = state.engine.get_or_prepare().await;
    Ok(Json(json!({ "briefing": briefing })))
}

async fn refresh(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    let briefing = state.engine.prepare().await;
    Ok(Json(json!({ "briefing": briefing })))
}

#[derive(Deserialize)]
struct PlayMusicRequest {
    uri: String,
}

async fn play_music(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<PlayMusicRequest>,
) -> Reply {
    authorize(&state, &headers)?;
    match state.engine.play_staged_music(&body.uri).await {
        Ok(result) => Ok(Json(json!({ "ok": true, "result": result }))),
        Err(err) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": err })),
        )),
    }
}

async fn dismiss(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    state.engine.dismiss();
    Ok(Json(json!({ "dismissed": true })))
}
