//! SAIL Tracker owner API, nested at `/api/tracker`.
//!
//! Every route needs the owner bearer token. `GET /stream` is Server-Sent Events read with
//! `fetch` (so the token travels in a header): a `board` event with everything, then
//! `patch` events carrying only the fields a refresh changed.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::stream::{self, Stream};
use sail_guardian::authority::ApprovalAuthority;
use sail_tracker::{Source, Tracker, TrackerConfig, TrackerError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

struct TrackerState {
    tracker: Arc<Tracker>,
    authority: Option<ApprovalAuthority>,
}

type Failure = (StatusCode, Json<Value>);

pub fn router(tracker: Arc<Tracker>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/board", get(board))
        .route("/config", get(config).put(save_config))
        .route("/refresh", post(refresh))
        .route("/geocode", get(geocode))
        .route("/stream", get(stream_board))
        .route("/tiles/:z/:x/:y", get(tile))
        .layer(axum::extract::DefaultBodyLimit::max(128_000))
        .with_state(Arc::new(TrackerState { tracker, authority }))
}

/// The tracker page and its assets, mounted at the site root.
pub fn pages<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route(
            "/tracker",
            get(|| async { axum::response::Html(include_str!("../web/tracker.html")) }),
        )
        .route(
            "/tracker.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/tracker.js"),
                )
            }),
        )
        .route(
            "/tracker.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/tracker.css"),
                )
            }),
        )
}

fn authorize(s: &TrackerState, h: &HeaderMap) -> Result<(), Failure> {
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
    s.tracker.touch();
    Ok(())
}

fn failure(e: TrackerError) -> Failure {
    match e {
        TrackerError::Invalid(message) => {
            (StatusCode::BAD_REQUEST, Json(json!({"error": message})))
        }
        TrackerError::Fetch(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("The public source did not answer: {e}")})),
        ),
        TrackerError::Store(e) => {
            tracing::warn!(error = %e, "Tracker storage error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"Tracker storage is unavailable."})),
            )
        }
    }
}

fn no_store(value: Value) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

fn full(tracker: &Tracker) -> Value {
    json!({
        "config": tracker.config(),
        "board": tracker.board(),
        "server_time": chrono::Utc::now().timestamp(),
    })
}

async fn board(State(s): State<Arc<TrackerState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    Ok(no_store(full(&s.tracker)))
}

async fn config(State(s): State<Arc<TrackerState>>, h: HeaderMap) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    Ok(no_store(json!(s.tracker.config())))
}

async fn save_config(
    State(s): State<Arc<TrackerState>>,
    h: HeaderMap,
    body: Result<Json<TrackerConfig>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let Json(config) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid settings: {}", e.body_text())})),
        )
    })?;
    let saved = s.tracker.set_config(config).map_err(failure)?;
    Ok(no_store(json!(saved)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshRequest {
    #[serde(default)]
    source: Option<String>,
}

async fn refresh(
    State(s): State<Arc<TrackerState>>,
    h: HeaderMap,
    Json(body): Json<RefreshRequest>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let sources = match body.source.as_deref() {
        None => Source::ALL.to_vec(),
        Some(name) => vec![Source::parse(name).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Unknown source: {name}")})),
            )
        })?],
    };
    s.tracker.mark_due(&sources);
    Ok((StatusCode::ACCEPTED, Json(json!({"queued": sources}))).into_response())
}

#[derive(Deserialize)]
struct GeocodeQuery {
    q: String,
}

async fn geocode(
    State(s): State<Arc<TrackerState>>,
    h: HeaderMap,
    Query(query): Query<GeocodeQuery>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let results = s.tracker.geocode(&query.q).await.map_err(failure)?;
    Ok(no_store(json!({"results": results})))
}

async fn tile(
    State(s): State<Arc<TrackerState>>,
    h: HeaderMap,
    Path((z, x, y)): Path<(u32, u32, String)>,
) -> Result<Response, Failure> {
    authorize(&s, &h)?;
    let y: u32 = y
        .strip_suffix(".png")
        .unwrap_or(&y)
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, Json(json!({"error":"Bad tile"}))))?;
    let bytes = s.tracker.tile(z, x, y).await.map_err(failure)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    )
        .into_response())
}

async fn stream_board(
    State(s): State<Arc<TrackerState>>,
    h: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Failure> {
    authorize(&s, &h)?;
    let updates = s.tracker.subscribe();
    let first = full(&s.tracker);
    let tracker = s.tracker.clone();
    let events = stream::unfold(
        (Some(first), updates, tracker),
        |(pending, mut updates, tracker)| async move {
            if let Some(board) = pending {
                let event = Event::default().event("board").data(board.to_string());
                return Some((Ok(event), (None, updates, tracker)));
            }
            match updates.recv().await {
                Ok(patch) => {
                    let event = Event::default().event("patch").data(patch.to_string());
                    Some((Ok(event), (None, updates, tracker)))
                }
                // A slow reader missed patches: resend everything.
                Err(RecvError::Lagged(_)) => {
                    let event = Event::default()
                        .event("board")
                        .data(full(&tracker).to_string());
                    Some((Ok(event), (None, updates, tracker)))
                }
                Err(RecvError::Closed) => None,
            }
        },
    );
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

    fn app() -> Router {
        let tracker =
            Tracker::with_store(sail_tracker::store::Store::open_in_memory().unwrap()).unwrap();
        router(tracker, Some(ApprovalAuthority::new(TOKEN).unwrap()))
    }

    fn req(method: &str, uri: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        }
    }

    async fn json_body(r: Response) -> Value {
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn requires_owner_token() {
        let r = app()
            .oneshot(req("GET", "/board", None, None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let r = app()
            .oneshot(req(
                "GET",
                "/board",
                None,
                Some("wrong-token-wrong-token-wrong-token"),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn board_and_config_round_trip() {
        let app = app();
        let r = app
            .clone()
            .oneshot(req("GET", "/board", None, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v = json_body(r).await;
        assert!(v["config"]["places"].is_array());
        assert!(v["board"]["sources"]["news"].is_object());

        let mut config = v["config"].clone();
        config["topics"] = json!(["shipping"]);
        let r = app
            .clone()
            .oneshot(req("PUT", "/config", Some(config), Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(json_body(r).await["topics"], json!(["shipping"]));

        let mut bad = v["config"].clone();
        bad["feeds"] = json!([{"name":"x","url":"ftp://example.com"}]);
        let r = app
            .clone()
            .oneshot(req("PUT", "/config", Some(bad), Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);

        let r = app
            .oneshot(req(
                "PUT",
                "/config",
                Some(json!({"surprise":1})),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn refresh_validates_source_and_tiles_validate_range() {
        let app = app();
        let r = app
            .clone()
            .oneshot(req(
                "POST",
                "/refresh",
                Some(json!({"source":"nope"})),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = app
            .clone()
            .oneshot(req(
                "POST",
                "/refresh",
                Some(json!({"source":"quotes"})),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
        let r = app
            .oneshot(req("GET", "/tiles/1/5/0.png", None, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
