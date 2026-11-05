//! Spotify for the interface, nested at `/api/spotify`.
//!
//! The status is readable like the host metrics; setting up, signing in, controlling playback
//! and disconnecting need the owner token. The sign-in callback is protected by the single-use
//! PKCE `state` instead, because Spotify's redirect cannot carry the token.
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Html,
    routing::{get, post},
};
use sail_bridge::spotify::{Spotify, SpotifyError};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct HttpState {
    spotify: Arc<Spotify>,
    authority: Option<ApprovalAuthority>,
}

pub fn router(authority: Option<ApprovalAuthority>) -> Router {
    router_with(sail_bridge::spotify::global(), authority)
}

pub fn router_with(spotify: Arc<Spotify>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/setup", post(setup))
        .route("/login", post(login))
        .route("/callback", get(callback))
        .route("/control", post(control))
        .route("/disconnect", post(disconnect))
        .layer(axum::extract::DefaultBodyLimit::max(4_096))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(Arc::new(HttpState { spotify, authority }))
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn authorize(state: &HttpState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let refuse = |status| (status, Json(json!({"error":"owner token required"})));
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

fn failure(error: SpotifyError) -> (StatusCode, Json<Value>) {
    let status = match error.code {
        "invalid" => StatusCode::BAD_REQUEST,
        "not_configured" | "not_connected" | "no_device" | "premium_required" | "expired" => {
            StatusCode::CONFLICT
        }
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        "auth" | "forbidden" => StatusCode::FORBIDDEN,
        _ => StatusCode::BAD_GATEWAY,
    };
    (
        status,
        Json(json!({"error": error.message, "code": error.code})),
    )
}

/// The redirect Spotify sends the browser back to: this server, as the browser reached it.
fn callback_uri(headers: &HeaderMap) -> Option<String> {
    let host = headers.get("host")?.to_str().ok()?;
    let valid = !host.is_empty()
        && host.len() <= 255
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b));
    valid.then(|| format!("https://{host}/api/spotify/callback"))
}

async fn status(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Json<Value> {
    let spotify = &state.spotify;
    let configured = spotify.configured();
    let connected = configured && spotify.connected().await;
    let mut body = json!({
        "configured": configured,
        "connected": connected,
        "redirect_uri": if configured { spotify.redirect_uri() } else { callback_uri(&headers).unwrap_or_default() },
        "now_playing": Value::Null,
    });
    if connected {
        match spotify.now_playing().await {
            Ok(now) => body["now_playing"] = now,
            Err(error) => {
                body["error"] = json!(error.message);
                body["code"] = json!(error.code);
            }
        }
    }
    Json(body)
}

#[derive(Deserialize)]
struct Setup {
    client_id: String,
}

async fn setup(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<Setup>,
) -> Reply {
    authorize(&state, &headers)?;
    let redirect = callback_uri(&headers).ok_or((
        StatusCode::BAD_REQUEST,
        Json(json!({"error":"Open SAIL by its address to set up Spotify."})),
    ))?;
    state
        .spotify
        .configure(&body.client_id, &redirect)
        .map_err(failure)?;
    Ok(Json(json!({"configured": true, "redirect_uri": redirect})))
}

async fn login(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    let url = state.spotify.login_url().map_err(failure)?;
    Ok(Json(json!({"url": url})))
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn page(title: &str, message: &str, ok: bool) -> Html<String> {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    Html(format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
<title>{t}</title><body style=\"font:16px system-ui;background:#141619;color:#eceff1;display:grid;place-items:center;min-height:100vh;margin:0\">\
<main style=\"max-width:28rem;padding:24px;border:1px solid #2c3136;border-radius:12px;background:#1b1e22\"><h1 style=\"font-size:20px;margin:0 0 8px\">{t}</h1><p style=\"margin:0;color:#c3c9ce\">{m}</p></main>\
<script>try{{window.opener&&window.opener.postMessage({{type:'sail-spotify',ok:{ok}}},location.origin)}}catch(_){{}}{close}</script></body></html>",
        t = escape(title),
        m = escape(message),
        close = if ok {
            "setTimeout(()=>window.close(),1500)"
        } else {
            ""
        },
    ))
}

async fn callback(
    State(state): State<Arc<HttpState>>,
    Query(query): Query<Callback>,
) -> (StatusCode, Html<String>) {
    if let Some(error) = query.error {
        let message = if error == "access_denied" {
            "You declined the Spotify sign-in. Nothing was connected.".to_string()
        } else {
            format!("Spotify did not complete the sign-in ({error}).")
        };
        return (
            StatusCode::OK,
            page("Spotify not connected", &message, false),
        );
    }
    let (Some(code), Some(login)) = (query.code, query.state) else {
        return (
            StatusCode::BAD_REQUEST,
            page(
                "Spotify not connected",
                "This sign-in link is incomplete.",
                false,
            ),
        );
    };
    match state.spotify.finish_login(&login, &code).await {
        Ok(()) => (
            StatusCode::OK,
            page(
                "Spotify connected",
                "SAIL can now play your music. You can close this tab.",
                true,
            ),
        ),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            page("Spotify not connected", &error.message, false),
        ),
    }
}

#[derive(Deserialize)]
struct Control {
    action: String,
}

async fn control(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<Control>,
) -> Reply {
    authorize(&state, &headers)?;
    state.spotify.control(&body.action).await.map_err(failure)?;
    let now = state.spotify.now_playing().await.unwrap_or(Value::Null);
    Ok(Json(json!({"now_playing": now})))
}

async fn disconnect(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    state.spotify.disconnect().await.map_err(failure)?;
    Ok(Json(json!({"connected": false})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

    fn app() -> (Router, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sail-spotify-http-{}", uuid::Uuid::new_v4()));
        let spotify = Arc::new(Spotify::with_endpoints(
            dir.clone(),
            "http://127.0.0.1:9",
            "http://127.0.0.1:9",
        ));
        (
            router_with(spotify, ApprovalAuthority::new(TOKEN).ok()),
            dir,
        )
    }

    async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn status_reports_setup_state_and_the_redirect_to_register() {
        let (app, dir) = app();
        let (status, body) = send(
            &app,
            Request::get("/status")
                .header("host", "127.0.0.1:8088")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["configured"], false);
        assert_eq!(body["connected"], false);
        assert_eq!(
            body["redirect_uri"],
            "https://127.0.0.1:8088/api/spotify/callback"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn changes_need_the_owner_token() {
        let (app, dir) = app();
        for path in ["/setup", "/login", "/control", "/disconnect"] {
            let (status, _) = send(
                &app,
                Request::post(path)
                    .header("host", "127.0.0.1:8088")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"client_id":"x","action":"pause"}"#))
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn setup_then_login_returns_a_pkce_sign_in_url() {
        let (app, dir) = app();
        let post = |path: &str, body: &str| {
            Request::post(path)
                .header("host", "127.0.0.1:8088")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let (status, body) = send(
            &app,
            post(
                "/setup",
                r#"{"client_id":"0123456789abcdef0123456789abcdef"}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = send(&app, post("/login", "{}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let url = body["url"].as_str().unwrap();
        assert!(
            url.contains("redirect_uri=https%3A%2F%2F127.0.0.1%3A8088%2Fapi%2Fspotify%2Fcallback")
        );
        assert!(url.contains("code_challenge="));
        // A forged or replayed callback is refused.
        let response = app
            .clone()
            .oneshot(
                Request::get("/callback?code=abc&state=forged")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(dir);
    }
}
