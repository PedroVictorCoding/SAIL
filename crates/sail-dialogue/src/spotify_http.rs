//! Spotify for the interface, nested at `/api/spotify`.
//!
//! The status is readable like the host metrics; setting up, signing in, controlling playback
//! and disconnecting need the owner token. The sign-in callback is protected by the single-use
//! PKCE `state` instead, because Spotify's redirect cannot carry the token.
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, Uri},
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
        .route("/player/token", post(player_token))
        .route("/player/play", post(player_play))
        .route("/player/search", get(player_search))
        .route("/player/tracks", get(player_tracks))
        .route("/player/playlists", get(player_playlists))
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
        "not_configured" | "not_connected" | "no_device" | "premium_required" | "expired"
        | "reconnect" | "not_owner" => StatusCode::CONFLICT,
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
/// HTTP/2 carries the address in the request's `:authority` (the URI), not a Host header.
fn callback_uri(uri: &Uri, headers: &HeaderMap) -> Option<String> {
    let host = match uri.authority() {
        Some(authority) => authority.as_str(),
        None => headers.get("host")?.to_str().ok()?,
    };
    let valid = !host.is_empty()
        && host.len() <= 255
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b));
    valid.then(|| format!("https://{host}/api/spotify/callback"))
}

async fn status(State(state): State<Arc<HttpState>>, uri: Uri, headers: HeaderMap) -> Json<Value> {
    let spotify = &state.spotify;
    let configured = spotify.configured();
    let connected = configured && spotify.connected().await;
    let mut body = json!({
        "configured": configured,
        "connected": connected,
        "redirect_uri": if configured { spotify.redirect_uri() } else { callback_uri(&uri, &headers).unwrap_or_default() },
        "now_playing": Value::Null,
        "can_stream": connected && spotify.can_stream().await,
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
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Setup>,
) -> Reply {
    authorize(&state, &headers)?;
    let redirect = callback_uri(&uri, &headers).ok_or((
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
        Ok(()) => {
            // Learn the owner's playlists now, so the first request is fast.
            state.spotify.playlist_index();
            (
                StatusCode::OK,
                page(
                    "Spotify connected",
                    "SAIL can now play your music. You can close this tab.",
                    true,
                ),
            )
        }
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

    /// Browsers on HTTP/2 (SAIL's TLS default) send the address as `:authority`, which
    /// reaches the handler as the URI's authority, with no Host header.
    #[tokio::test]
    async fn http2_requests_without_a_host_header_can_set_up() {
        let (routes, dir) = app();
        // Mounted as in the server, so the nesting's URI rewrite is covered too.
        let app = Router::new().nest_service("/api/spotify", routes);
        let request = |method: &str, body: &str| {
            Request::builder()
                .method(method)
                .uri(if method == "GET" {
                    "https://192.168.1.101:8088/api/spotify/status"
                } else {
                    "https://192.168.1.101:8088/api/spotify/setup"
                })
                .version(axum::http::Version::HTTP_2)
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let (_, body) = send(&app, request("GET", "")).await;
        assert_eq!(
            body["redirect_uri"],
            "https://192.168.1.101:8088/api/spotify/callback"
        );
        let (status, body) = send(
            &app,
            request(
                "POST",
                r#"{"client_id":"672012510a8c4af3979634a778f2f939"}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["redirect_uri"],
            "https://192.168.1.101:8088/api/spotify/callback"
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

// ---------------------------------------------------------------- the SAIL music player
// The in-browser player (Spotify Web Playback SDK) needs an access token in the browser; it
// is handed only to the owner, never cached by the browser (`no-store`), and refused when
// the sign-in lacks the `streaming` scope. Playback targets only the device id the browser
// player registered, with explicit track URIs, so SAIL's queue decides what plays next.

async fn player_token(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    let (token, expires_at) = state.spotify.player_token().await.map_err(failure)?;
    Ok(Json(
        json!({"access_token": token, "expires_at": expires_at}),
    ))
}

#[derive(Deserialize)]
struct PlayOn {
    device_id: String,
    /// Exact tracks, in SAIL's order.
    #[serde(default)]
    uris: Vec<String>,
    /// Or one album/playlist played as a whole (playlists whose songs Spotify won't share).
    #[serde(default)]
    context_uri: Option<String>,
    #[serde(default)]
    position_ms: u64,
}

async fn player_play(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<PlayOn>,
) -> Reply {
    authorize(&state, &headers)?;
    if let Some(context) = &body.context_uri {
        if !body.uris.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Send either uris or context_uri, not both."})),
            ));
        }
        state
            .spotify
            .play_context_on(&body.device_id, context)
            .await
            .map_err(failure)?;
        return Ok(Json(json!({"playing": context})));
    }
    state
        .spotify
        .play_on(&body.device_id, &body.uris, body.position_ms)
        .await
        .map_err(failure)?;
    Ok(Json(json!({"playing": body.uris})))
}

#[derive(Deserialize)]
struct Search {
    q: String,
}

async fn player_search(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(query): Query<Search>,
) -> Reply {
    authorize(&state, &headers)?;
    let q = query.q.trim();
    if q.is_empty() || q.chars().count() > 200 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Search for 1 to 200 characters."})),
        ));
    }
    let tracks = state
        .spotify
        .search_tracks(q, sail_bridge::spotify::SEARCH_LIMIT)
        .await
        .map_err(failure)?;
    Ok(Json(json!({"tracks": tracks})))
}

#[derive(Deserialize)]
struct Of {
    uri: String,
}

async fn player_tracks(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(query): Query<Of>,
) -> Reply {
    authorize(&state, &headers)?;
    let tracks = state.spotify.tracks_of(&query.uri).await.map_err(failure)?;
    Ok(Json(json!({"tracks": tracks})))
}

async fn player_playlists(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Reply {
    authorize(&state, &headers)?;
    let lists = state.spotify.my_playlists().await.map_err(failure)?;
    Ok(Json(json!({"playlists": lists})))
}

#[cfg(test)]
mod player_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Mutex as StdMutex;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

    /// A fake Spotify Web API that records play requests.
    async fn fake_api(seen: Arc<StdMutex<Vec<(String, Value)>>>) -> String {
        let record = seen.clone();
        let api = Router::new()
            .route(
                "/me/player/play",
                axum::routing::put(move |uri: Uri, body: Json<Value>| {
                    let record = record.clone();
                    async move {
                        record.lock().unwrap().push((uri.to_string(), body.0));
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/playlists/:id/items",
                get(|axum::extract::Path(id): axum::extract::Path<String>| async move {
                    if id == "followedplaylist0000000" {
                        return (StatusCode::FORBIDDEN, Json(json!({"error": {"status": 403, "message": "Forbidden"}})));
                    }
                    (StatusCode::OK, Json(json!({"items": [
                        {"added_at": "x", "is_local": false, "item": {"type": "track", "track": true, "episode": false,
                            "uri": "spotify:track:3333333333333333333333", "name": "Mine", "duration_ms": 3000,
                            "artists": [{"name": "Me"}], "album": {"name": "Own", "images": [{"url": "https://i.scdn.co/image/own"}]}}},
                        {"added_at": "x", "is_local": false, "item": {"type": "episode", "uri": "spotify:episode:abc", "name": "Pod"}}
                    ], "next": null})))
                }),
            )
            .route(
                "/playlists/:id/tracks",
                get(|| async { (StatusCode::FORBIDDEN, Json(json!({"error": {"status": 403, "message": "Forbidden"}}))) }),
            )
            .route(
                "/search",
                get(|Query(q): Query<std::collections::HashMap<String, String>>| async move {
                    assert!(q["limit"].parse::<usize>().unwrap() <= 10, "Spotify caps search at 10");
                    Json(json!({"tracks": {"items": [
                        {"uri": "spotify:track:4uLU6hMCjMI75M1A2tKUQC", "name": "Song", "duration_ms": 200000,
                         "artists": [{"name": "A"}, {"name": "B"}], "album": {"name": "LP", "images": [{"url": "https://i.scdn.co/image/big"}, {"url": "https://i.scdn.co/image/small"}]}},
                        {"uri": "spotify:episode:xyz", "name": "Podcast"},
                        {"uri": "spotify:local:a:b:c:1", "name": "Local", "is_local": true}
                    ]}}))
                }),
            )
            .route(
                "/albums/:id",
                get(|| async {
                    Json(json!({"name": "LP", "images": [{"url": "https://i.scdn.co/image/lp"}], "tracks": {"items": [
                        {"uri": "spotify:track:1111111111111111111111", "name": "One", "duration_ms": 1000, "artists": [{"name": "A"}]},
                        {"uri": "spotify:track:2222222222222222222222", "name": "Two", "duration_ms": 2000, "artists": [{"name": "A"}]}
                    ]}}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
        format!("http://{addr}")
    }

    fn connected(dir: &std::path::Path, scope: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"client_id":"672012510a8c4af3979634a778f2f939","redirect_uri":"https://sail.test/api/spotify/callback"}"#).unwrap();
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3000;
        std::fs::write(
            dir.join("token.json"),
            json!({"access_token": "access-1", "refresh_token": "refresh-1", "expires_at": expires, "scope": scope}).to_string(),
        )
        .unwrap();
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

    fn owner(method: &str, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn browser_player_token_needs_owner_and_streaming_scope() {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let api = fake_api(seen).await;
        let dir =
            std::env::temp_dir().join(format!("sail-spotify-player-{}", uuid::Uuid::new_v4()));
        connected(&dir, "user-read-playback-state user-modify-playback-state");
        let app = router_with(
            Arc::new(Spotify::with_endpoints(dir.clone(), &api, &api)),
            ApprovalAuthority::new(TOKEN).ok(),
        );
        let (status, _) = send(
            &app,
            Request::post("/player/token").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = send(&app, owner("POST", "/player/token", "")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "reconnect");
        let (_, body) = send(
            &app,
            Request::get("/status")
                .header("host", "sail.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(body["can_stream"], false);

        connected(
            &dir,
            "user-read-playback-state streaming user-read-email user-read-private",
        );
        let app = router_with(
            Arc::new(Spotify::with_endpoints(dir.clone(), &api, &api)),
            ApprovalAuthority::new(TOKEN).ok(),
        );
        let (status, body) = send(&app, owner("POST", "/player/token", "")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["access_token"], "access-1");
        assert!(body["expires_at"].as_u64().unwrap() > 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn plays_tracks_on_the_browser_device_and_maps_search_and_albums() {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let api = fake_api(seen.clone()).await;
        let dir =
            std::env::temp_dir().join(format!("sail-spotify-player-{}", uuid::Uuid::new_v4()));
        connected(&dir, "streaming");
        let app = router_with(
            Arc::new(Spotify::with_endpoints(dir.clone(), &api, &api)),
            ApprovalAuthority::new(TOKEN).ok(),
        );

        let (status, body) = send(
            &app,
            owner("POST", "/player/play", r#"{"device_id":"abc123def456","uris":["spotify:track:4uLU6hMCjMI75M1A2tKUQC"],"position_ms":5000}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0]
                .0
                .ends_with("/me/player/play?device_id=abc123def456"),
            "{}",
            calls[0].0
        );
        assert_eq!(
            calls[0].1,
            json!({"uris": ["spotify:track:4uLU6hMCjMI75M1A2tKUQC"], "position_ms": 5000})
        );
        // Only track URIs, only a plausible device id.
        for bad in [
            r#"{"device_id":"abc123def456","uris":["spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"]}"#,
            r#"{"device_id":"../../me","uris":["spotify:track:4uLU6hMCjMI75M1A2tKUQC"]}"#,
            r#"{"device_id":"abc123def456","uris":[]}"#,
        ] {
            let (status, _) = send(&app, owner("POST", "/player/play", bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        }
        assert_eq!(seen.lock().unwrap().len(), 1);

        let (status, body) = send(&app, owner("GET", "/player/search?q=song", "")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let tracks = body["tracks"].as_array().unwrap();
        assert_eq!(
            tracks.len(),
            1,
            "episodes and local files are skipped: {body}"
        );
        assert_eq!(
            tracks[0],
            json!({"source": "spotify", "id": "spotify:track:4uLU6hMCjMI75M1A2tKUQC", "title": "Song",
            "artist": "A, B", "album": "LP", "duration_ms": 200000, "art": "https://i.scdn.co/image/big"})
        );

        let (status, body) = send(
            &app,
            owner(
                "GET",
                "/player/tracks?uri=spotify:album:4aawyAB9vmqN3uQ7FjRGTy",
                "",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let tracks = body["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[1]["title"], "Two");
        assert_eq!(tracks[1]["album"], "LP");
        assert_eq!(tracks[1]["art"], "https://i.scdn.co/image/lp");
        let (status, _) = send(
            &app,
            owner(
                "GET",
                "/player/tracks?uri=spotify:track:4uLU6hMCjMI75M1A2tKUQC",
                "",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Spotify, February 2026: playlist songs come from `/items` (rows hold `item`) and only
    /// for the owner's playlists; followed playlists are played as a whole instead.
    #[tokio::test]
    async fn playlists_use_items_and_followed_ones_play_as_a_whole() {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let api = fake_api(seen.clone()).await;
        let dir =
            std::env::temp_dir().join(format!("sail-spotify-player-{}", uuid::Uuid::new_v4()));
        connected(&dir, "streaming");
        let app = router_with(
            Arc::new(Spotify::with_endpoints(dir.clone(), &api, &api)),
            ApprovalAuthority::new(TOKEN).ok(),
        );
        let (status, body) = send(
            &app,
            owner(
                "GET",
                "/player/tracks?uri=spotify:playlist:ownplaylist000000000000",
                "",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["tracks"],
            json!([{"source": "spotify", "id": "spotify:track:3333333333333333333333", "title": "Mine",
            "artist": "Me", "album": "Own", "duration_ms": 3000, "art": "https://i.scdn.co/image/own"}])
        );
        let (status, body) = send(
            &app,
            owner(
                "GET",
                "/player/tracks?uri=spotify:playlist:followedplaylist0000000",
                "",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "not_owner");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("play this playlist as a whole")
        );

        let (status, body) = send(&app, owner("POST", "/player/play",
            r#"{"device_id":"abc123def456","context_uri":"spotify:playlist:followedplaylist0000000"}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            seen.lock().unwrap().last().unwrap().1,
            json!({"context_uri": "spotify:playlist:followedplaylist0000000"})
        );
        for bad in [
            r#"{"device_id":"abc123def456","context_uri":"spotify:track:4uLU6hMCjMI75M1A2tKUQC"}"#,
            r#"{"device_id":"abc123def456","context_uri":"spotify:playlist:followedplaylist0000000","uris":["spotify:track:4uLU6hMCjMI75M1A2tKUQC"]}"#,
        ] {
            let (status, _) = send(&app, owner("POST", "/player/play", bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Spotify answers 503 intermittently: reads retry once, changes are never repeated.
    #[tokio::test]
    async fn transient_503_is_retried_for_reads_only() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let lists = Arc::new(AtomicUsize::new(0));
        let plays = Arc::new(AtomicUsize::new(0));
        let (l, pl) = (lists.clone(), plays.clone());
        let api = Router::new()
            .route(
                "/me/playlists",
                get(move || {
                    let l = l.clone();
                    async move {
                        if l.fetch_add(1, Ordering::SeqCst) == 0 {
                            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
                        }
                        (StatusCode::OK, Json(json!({"items": [{"name": "Mine", "uri": "spotify:playlist:ownplaylist000000000000", "owner": {"display_name": "Pedro"}}], "next": null})))
                    }
                }),
            )
            .route(
                "/me/player/play",
                axum::routing::put(move || {
                    let pl = pl.clone();
                    async move {
                        pl.fetch_add(1, Ordering::SeqCst);
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
        let base = format!("http://{addr}");
        let dir =
            std::env::temp_dir().join(format!("sail-spotify-player-{}", uuid::Uuid::new_v4()));
        connected(&dir, "streaming");
        let app = router_with(
            Arc::new(Spotify::with_endpoints(dir.clone(), &base, &base)),
            ApprovalAuthority::new(TOKEN).ok(),
        );
        let (status, body) = send(&app, owner("GET", "/player/playlists", "")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["playlists"][0]["name"], "Mine");
        assert_eq!(lists.load(Ordering::SeqCst), 2);
        let (status, _) = send(
            &app,
            owner(
                "POST",
                "/player/play",
                r#"{"device_id":"abc123def456","uris":["spotify:track:4uLU6hMCjMI75M1A2tKUQC"]}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(plays.load(Ordering::SeqCst), 1, "a change is sent once");
        let _ = std::fs::remove_dir_all(dir);
    }
}
