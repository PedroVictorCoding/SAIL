//! SAIL Music's owner API for uploaded (bought, DRM-free) music, nested at `/api/music`.
//!
//! Library routes need the owner bearer token. An `<audio>` element cannot send that header,
//! so playback goes through a stream ticket: the owner asks `POST /tracks/:id/stream` for a
//! URL, and `GET /stream/:ticket` serves that one track with HTTP Range support (seeking)
//! until the ticket expires. Tickets are random, bound to one track, few and short-lived.
//! Spotify lives under `/api/spotify/player/*`; the browser player joins both sources.
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use sail_bridge::music::{Library, MusicError, Track};
use sail_guardian::authority::ApprovalAuthority;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A ticket stays valid long enough for a paused album to be resumed and seeked.
pub const TICKET_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_TICKETS: usize = 256;
const CHUNK: usize = 64 * 1024;

struct MusicState {
    library: Arc<Library>,
    authority: Option<ApprovalAuthority>,
    tickets: Mutex<HashMap<String, (String, Instant)>>,
}

type Failure = (StatusCode, Json<Value>);

pub fn routes(authority: Option<ApprovalAuthority>) -> Option<Router> {
    match sail_bridge::music::global() {
        Ok(library) => Some(router(library, authority)),
        Err(error) => {
            tracing::warn!(%error, "Music library unavailable; uploads disabled");
            None
        }
    }
}

pub fn router(library: Arc<Library>, authority: Option<ApprovalAuthority>) -> Router {
    let state = Arc::new(MusicState {
        library,
        authority,
        tickets: Mutex::new(HashMap::new()),
    });
    Router::new()
        .route("/library", get(library_list))
        .route(
            "/import",
            post(import).layer(axum::extract::DefaultBodyLimit::max(
                sail_bridge::music::MAX_IMPORT_BYTES + 1,
            )),
        )
        .route("/tracks/:id", delete(remove))
        .route("/tracks/:id/cover", get(cover))
        .route("/tracks/:id/stream", post(ticket))
        .route("/stream/:ticket", get(stream))
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                response
            },
        ))
        .with_state(state)
}

fn authorize(state: &MusicState, headers: &HeaderMap) -> Result<(), Failure> {
    let deny = |code| (code, Json(json!({"error":"Owner authentication required"})));
    let authority = state
        .authority
        .as_ref()
        .ok_or_else(|| deny(StatusCode::SERVICE_UNAVAILABLE))?;
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| deny(StatusCode::UNAUTHORIZED))?;
    if !authority.authenticates(token) {
        return Err(deny(StatusCode::UNAUTHORIZED));
    }
    Ok(())
}

fn failure(error: MusicError) -> Failure {
    let status = match &error {
        MusicError::Invalid(_) => StatusCode::BAD_REQUEST,
        MusicError::Protected | MusicError::Unsupported => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        MusicError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        MusicError::Full => StatusCode::INSUFFICIENT_STORAGE,
        MusicError::NotFound => StatusCode::NOT_FOUND,
        MusicError::Io(e) => {
            tracing::warn!(error = %e, "music storage failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, Json(json!({"error": error.to_string()})))
}

/// A track as the player sees it: one item shape for uploads and Spotify.
fn item(track: &Track) -> Value {
    json!({
        "source": "local",
        "id": track.id,
        "title": track.title,
        "artist": track.artist,
        "album": track.album,
        "album_artist": track.album_artist,
        "track_number": track.track_number,
        "disc_number": track.disc_number,
        "year": track.year,
        "duration_ms": track.duration_ms,
        "format": track.format,
        "has_cover": track.has_cover,
        "added_at": track.added_at,
    })
}

async fn library_list(
    State(state): State<Arc<MusicState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let tracks: Vec<Value> = state.library.list().iter().map(item).collect();
    Ok(Json(json!({ "tracks": tracks })))
}

async fn import(
    State(state): State<Arc<MusicState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let name = headers
        .get("x-filename")
        .and_then(|v| v.to_str().ok())
        .map(|v| percent_decode(v.trim()))
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .ok_or_else(|| {
            failure(MusicError::Invalid(
                "Send the file name in the X-Filename header.".into(),
            ))
        })?;
    let library = state.library.clone();
    let (track, new) = tokio::task::spawn_blocking(move || library.import(&name, &body))
        .await
        .map_err(|_| {
            failure(MusicError::Invalid(
                "The import stopped unexpectedly.".into(),
            ))
        })?
        .map_err(failure)?;
    Ok(Json(json!({"track": item(&track), "new": new})))
}

async fn remove(
    State(state): State<Arc<MusicState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    authorize(&state, &headers)?;
    let library = state.library.clone();
    let track = id.clone();
    tokio::task::spawn_blocking(move || library.remove(&track))
        .await
        .map_err(|_| failure(MusicError::NotFound))?
        .map_err(failure)?;
    state.tickets.lock().unwrap().retain(|_, (t, _)| *t != id);
    Ok(StatusCode::NO_CONTENT)
}

async fn cover(
    State(state): State<Arc<MusicState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    authorize(&state, &headers)?;
    let library = state.library.clone();
    let bytes = tokio::task::spawn_blocking(move || library.cover(&id))
        .await
        .map_err(|_| failure(MusicError::NotFound))?
        .map_err(failure)?;
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response())
}

async fn ticket(
    State(state): State<Arc<MusicState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    state.library.audio(&id).map_err(failure)?;
    let ticket = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut tickets = state.tickets.lock().unwrap();
    let now = Instant::now();
    tickets.retain(|_, (_, at)| now.duration_since(*at) < TICKET_TTL);
    let oldest = (tickets.len() >= MAX_TICKETS)
        .then(|| {
            tickets
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
        })
        .flatten();
    if let Some(oldest) = oldest {
        tickets.remove(&oldest);
    }
    tickets.insert(ticket.clone(), (id, now));
    Ok(Json(
        json!({"url": format!("/api/music/stream/{ticket}"), "expires_in": TICKET_TTL.as_secs()}),
    ))
}

/// The browser percent-encodes file names (headers are Latin-1); invalid UTF-8 is replaced.
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%' && i + 2 < bytes.len() && value.is_char_boundary(i + 3))
            .then(|| u8::from_str_radix(&value[i + 1..i + 3], 16).ok())
            .flatten();
        if let Some(b) = hex {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` against a file of `len` bytes; `None` when unsatisfiable.
pub fn parse_range(value: &str, len: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() {
            len - 1
        } else {
            b.parse::<u64>().ok()?.min(len - 1)
        };
        (start, end)
    };
    (start <= end && start < len).then_some((start, end))
}

async fn stream(
    State(state): State<Arc<MusicState>>,
    headers: HeaderMap,
    Path(ticket): Path<String>,
) -> Result<Response, Failure> {
    let id = {
        let tickets = state.tickets.lock().unwrap();
        match tickets.get(&ticket) {
            Some((id, at)) if at.elapsed() < TICKET_TTL => id.clone(),
            _ => {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(json!({"error":"This stream link has expired."})),
                ));
            }
        }
    };
    let (path, format, len) = state.library.audio(&id).map_err(failure)?;
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (status, start, end) = match range {
        None => (StatusCode::OK, 0, len.saturating_sub(1)),
        Some(value) => match parse_range(value, len) {
            Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e),
            None => {
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{len}"))],
                )
                    .into_response());
            }
        },
    };
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| failure(MusicError::NotFound))?;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| failure(e.into()))?;
    let remaining = if len == 0 { 0 } else { end - start + 1 };
    let body = futures_util::stream::unfold((file, remaining), |(mut file, left)| async move {
        if left == 0 {
            return None;
        }
        let mut buf = vec![0u8; CHUNK.min(left as usize)];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((
                    Ok::<_, std::io::Error>(Bytes::from(buf)),
                    (file, left - n as u64),
                ))
            }
            Err(e) => Some((Err(e), (file, 0))),
        }
    });
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, format.mime())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, remaining.to_string());
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    Ok(response
        .body(Body::from_stream(body))
        .expect("stream response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    const TOKEN: &str = "music-owner-token-0123456789abcdef";

    fn wav(ms: u32, title: &str) -> Vec<u8> {
        let rate = 8000u32;
        let data_len = rate * ms / 1000 * 2;
        let mut info = Vec::new();
        for (id, text) in [(b"INAM", title), (b"IART", "Test Artist")] {
            let mut v = text.as_bytes().to_vec();
            v.push(0);
            if v.len() % 2 == 1 {
                v.push(0);
            }
            info.extend_from_slice(id);
            info.extend_from_slice(&(v.len() as u32).to_le_bytes());
            info.extend_from_slice(&v);
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + data_len + 12 + info.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, 1u16] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&(4 + info.len() as u32).to_le_bytes());
        out.extend_from_slice(b"INFO");
        out.extend_from_slice(&info);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend((0..data_len).map(|i| (i % 251) as u8));
        out
    }

    fn app() -> (Router, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sail-music-http-{}", uuid::Uuid::new_v4()));
        let library = Arc::new(Library::open(&dir).unwrap());
        (router(library, ApprovalAuthority::new(TOKEN).ok()), dir)
    }

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    fn owner(method: &str, uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {TOKEN}"))
    }

    #[test]
    fn decodes_file_names() {
        assert_eq!(percent_decode("Caf%C3%A9%20Song.flac"), "Café Song.flac");
        assert_eq!(percent_decode("plain.mp3"), "plain.mp3");
        assert_eq!(percent_decode("100%.mp3"), "100%.mp3");
        assert_eq!(percent_decode("bad%zz.mp3"), "bad%zz.mp3");
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-500", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-1", 100), None);
        assert_eq!(parse_range("bytes=0-1,4-5", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }

    #[tokio::test]
    async fn owner_imports_lists_streams_with_ranges_and_removes() {
        let (app, dir) = app();
        let file = wav(800, "Night Drive");
        // Every library route needs the owner.
        let (status, _, _) =
            send(&app, Request::get("/library").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, body) = send(
            &app,
            owner("POST", "/import")
                .header("x-filename", "night.wav")
                .body(Body::from(file.clone()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: Value = serde_json::from_slice(&body).unwrap();
        let id = v["track"]["id"].as_str().unwrap().to_string();
        assert_eq!(v["track"]["title"], "Night Drive");
        assert_eq!(v["track"]["source"], "local");
        assert_eq!(v["new"], true);
        let (_, headers, body) =
            send(&app, owner("GET", "/library").body(Body::empty()).unwrap()).await;
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["tracks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        // Unsupported and protected files are refused with plain messages.
        let (status, _, _) = send(
            &app,
            owner("POST", "/import")
                .header("x-filename", "a.pdf")
                .body(Body::from("%PDF-1.7"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // Stream through a ticket, whole and by range.
        let (status, _, body) = send(
            &app,
            Request::post(format!("/tracks/{id}/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let (_, _, body) = send(
            &app,
            owner("POST", &format!("/tracks/{id}/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let url = serde_json::from_slice::<Value>(&body).unwrap()["url"]
            .as_str()
            .unwrap()
            .to_string();
        let path = url.strip_prefix("/api/music").unwrap().to_string();
        let (status, headers, body) =
            send(&app, Request::get(&path).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "audio/wav");
        assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
        assert_eq!(body, file);
        let (status, headers, body) = send(
            &app,
            Request::get(&path)
                .header("range", "bytes=100-199")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            headers[header::CONTENT_RANGE],
            format!("bytes 100-199/{}", file.len())
        );
        assert_eq!(body, file[100..200]);
        let (status, _, _) = send(
            &app,
            Request::get(&path)
                .header("range", format!("bytes={}-", file.len()))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        let (status, _, _) = send(
            &app,
            Request::get("/stream/not-a-ticket")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Removing the track revokes its tickets.
        let (status, _, _) = send(
            &app,
            owner("DELETE", &format!("/tracks/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = send(&app, Request::get(&path).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(dir);
    }
}
