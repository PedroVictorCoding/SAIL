//! SAIL Reader's owner API, nested at `/api/reader` (issue #333).
//!
//! Every route needs the owner bearer token. Books arrive as raw bytes with their name in
//! `X-Filename` and are parsed off the async runtime. Responses carry only plain-text blocks,
//! never book HTML. Nothing here reaches Mind or any model: reading is deterministic.
//!
//! `/speech` reads text aloud with the host's Kokoro worker (the one Dialogue uses), which on
//! this host is about ten times faster than Kokoro in a browser. The text is spoken literally.
//! Requests never open a voice turn, so narration does not cancel background work, and only one
//! is served at a time, so conversation speech waits for at most one sentence.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sail_bridge::reader::catalog::{Catalog, Metadata, MetadataEdits, SearchTerms, normalize_isbn};
use sail_bridge::reader::{Anchor, Book, Library, ReaderError};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
type Matches = HashMap<String, (Instant, Vec<Metadata>)>;
#[derive(Clone)]
struct MetadataPreview {
    token: String,
    created: Instant,
    metadata: Metadata,
    cover: Option<Arc<Vec<u8>>>,
}

struct ReaderState {
    library: Arc<Library>,
    authority: Option<ApprovalAuthority>,
    speech: Option<Arc<dyn Narrator>>,
    speaking: tokio::sync::Semaphore,
    catalog: Result<Catalog, String>,
    lookup_busy: tokio::sync::Semaphore,
    matches: Mutex<Matches>,
    previews: Mutex<HashMap<String, MetadataPreview>>,
}

/// Speaks text literally with a local voice. Implemented by the host voice services; tests
/// substitute a fake.
#[async_trait::async_trait]
pub trait Narrator: Send + Sync {
    async fn speak(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        speed: f32,
    ) -> Result<(Vec<i16>, u32), String>;
}

#[async_trait::async_trait]
impl Narrator for sail_mind::VoiceModelServices {
    async fn speak(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        speed: f32,
    ) -> Result<(Vec<i16>, u32), String> {
        self.synthesize_reading(text, voice, language, speed)
            .await
            .map_err(|e| e.to_string())
    }
}

/// The voices in the host's Kokoro v1.0 voice file (`models/voice/voices-v1.0.bin`).
pub const VOICES: [&str; 54] = [
    "af_alloy",
    "af_aoede",
    "af_bella",
    "af_heart",
    "af_jessica",
    "af_kore",
    "af_nicole",
    "af_nova",
    "af_river",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_echo",
    "am_eric",
    "am_fenrir",
    "am_liam",
    "am_michael",
    "am_onyx",
    "am_puck",
    "am_santa",
    "bf_alice",
    "bf_emma",
    "bf_isabella",
    "bf_lily",
    "bm_daniel",
    "bm_fable",
    "bm_george",
    "bm_lewis",
    "ef_dora",
    "em_alex",
    "em_santa",
    "ff_siwis",
    "hf_alpha",
    "hf_beta",
    "hm_omega",
    "hm_psi",
    "if_sara",
    "im_nicola",
    "jf_alpha",
    "jf_gongitsune",
    "jf_nezumi",
    "jf_tebukuro",
    "jm_kumo",
    "pf_dora",
    "pm_alex",
    "pm_santa",
    "zf_xiaobei",
    "zf_xiaoni",
    "zf_xiaoxiao",
    "zf_xiaoyi",
    "zm_yunjian",
    "zm_yunxi",
    "zm_yunxia",
    "zm_yunyang",
];
pub const MAX_SPEECH_CHARS: usize = 600;
pub const MIN_SPEED: f32 = 0.7;
pub const MAX_SPEED: f32 = 1.5;

/// Kokoro's language follows the voice's first letter.
fn voice_language(voice: &str) -> &'static str {
    match voice.as_bytes().first() {
        Some(b'b') => "en-gb",
        Some(b'e') => "es",
        Some(b'f') => "fr-fr",
        Some(b'h') => "hi",
        Some(b'i') => "it",
        Some(b'j') => "ja",
        Some(b'p') => "pt-br",
        Some(b'z') => "cmn",
        _ => "en-us",
    }
}

type Failure = (StatusCode, Json<Value>);

/// The reader routes, or `None` (logged) when the library cannot be opened.
pub fn routes(
    authority: Option<ApprovalAuthority>,
    speech: sail_mind::VoiceModelServices,
) -> Option<Router> {
    match sail_bridge::reader::global() {
        Ok(library) => Some(router_with_speech(
            library,
            authority,
            Some(Arc::new(speech)),
        )),
        Err(error) => {
            tracing::warn!(%error, "Reader library unavailable; reader disabled");
            None
        }
    }
}

pub fn router(library: Arc<Library>, authority: Option<ApprovalAuthority>) -> Router {
    router_with_speech(library, authority, None)
}

pub fn router_with_speech(
    library: Arc<Library>,
    authority: Option<ApprovalAuthority>,
    speech: Option<Arc<dyn Narrator>>,
) -> Router {
    router_with_state(Arc::new(ReaderState {
        library,
        authority,
        speech,
        speaking: tokio::sync::Semaphore::new(1),
        catalog: Catalog::new(),
        lookup_busy: tokio::sync::Semaphore::new(1),
        matches: Mutex::new(HashMap::new()),
        previews: Mutex::new(HashMap::new()),
    }))
}

fn router_with_state(state: Arc<ReaderState>) -> Router {
    Router::new()
        .route("/speech", get(speech_info).post(speak))
        .route("/library", get(library_list))
        .route("/import", post(import))
        .route("/books/:id", get(book).delete(remove))
        .route("/books/:id/sections/:index", get(section))
        .route("/books/:id/position", get(position).put(save_position))
        .route(
            "/books/:id/metadata/search",
            post(search_metadata).layer(axum::extract::DefaultBodyLimit::max(4096)),
        )
        .route(
            "/books/:id/metadata",
            post(select_metadata)
                .delete(clear_metadata)
                .layer(axum::extract::DefaultBodyLimit::max(16384)),
        )
        .route(
            "/books/:id/metadata/preview",
            post(preview_metadata).layer(axum::extract::DefaultBodyLimit::max(4096)),
        )
        .route(
            "/books/:id/metadata/preview/:token/cover",
            get(preview_cover),
        )
        .route("/books/:id/cover", get(cover))
        .layer(axum::extract::DefaultBodyLimit::max(
            sail_bridge::reader::MAX_IMPORT_BYTES + 1,
        ))
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(state)
}

fn authorize(state: &ReaderState, headers: &HeaderMap) -> Result<(), Failure> {
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

fn failure(error: ReaderError) -> Failure {
    let status = match &error {
        ReaderError::Invalid(_) | ReaderError::BadAnchor => StatusCode::BAD_REQUEST,
        ReaderError::Protected | ReaderError::Unsupported => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ReaderError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        ReaderError::Full => StatusCode::INSUFFICIENT_STORAGE,
        ReaderError::NotFound => StatusCode::NOT_FOUND,
        ReaderError::Stale(position) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": error.to_string(), "position": position})),
            );
        }
        ReaderError::Storage(e) => {
            tracing::warn!(error = %e, "Reader storage failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, Json(json!({"error": error.to_string()})))
}

/// The contract's `Book`: the stored record without its internal bookkeeping.
fn public(book: &Book) -> Value {
    json!({
        "id": book.id,
        "title": book.title,
        "authors": book.authors,
        "format": book.format,
        "language": book.language,
        "sections": book.sections,
        "chars": book.chars,
        "added_at": book.added_at,
        "opened_at": book.opened_at,
        "source_name": book.source_name,
        "position": book.position,
        "metadata": book.metadata.as_ref().map(|m| {
            let mut value = serde_json::to_value(m).expect("metadata is serializable");
            value["source_url"] = json!(m.source_url());
            value
        }),
    })
}

async fn library_list(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let books: Vec<Value> = state.library.list().iter().map(public).collect();
    Ok(Json(json!({ "books": books })))
}

async fn import(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Failure> {
    authorize(&state, &headers)?;
    let name = headers
        .get("x-filename")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            failure(ReaderError::Invalid(
                "Send the file name in the X-Filename header.".into(),
            ))
        })?;
    if body.is_empty() {
        return Err(failure(ReaderError::Invalid("The file is empty.".into())));
    }
    let library = state.library.clone();
    let imported = tokio::task::spawn_blocking(move || library.import(&name, &body))
        .await
        .map_err(|_| {
            failure(ReaderError::Invalid(
                "The import stopped unexpectedly.".into(),
            ))
        })?
        .map_err(failure)?;
    let status = if imported.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(json!({ "book": public(&imported.book) }))).into_response())
}

async fn book(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let library = state.library.clone();
    let (book, content) = tokio::task::spawn_blocking(move || library.open_book(&id))
        .await
        .map_err(|_| failure(ReaderError::NotFound))?
        .map_err(failure)?;
    Ok(Json(json!({
        "book": public(&book),
        "toc": content.toc,
        "sections": book.section_info,
    })))
}

async fn section(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path((id, index)): Path<(String, u32)>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let library = state.library.clone();
    let section = tokio::task::spawn_blocking(move || library.section(&id, index))
        .await
        .map_err(|_| failure(ReaderError::NotFound))?
        .map_err(failure)?;
    Ok(Json(json!({
        "index": index,
        "title": section.title,
        "blocks": section.blocks,
    })))
}

async fn position(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let book = state.library.book(&id).map_err(failure)?;
    Ok(Json(json!({ "position": book.position })))
}

#[derive(Deserialize)]
struct PositionBody {
    anchor: Anchor,
    percent: f32,
    expected_revision: u64,
}

async fn save_position(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<PositionBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let Json(body) = body.map_err(|_| {
        failure(ReaderError::Invalid(
            "Send { anchor, percent, expected_revision }.".into(),
        ))
    })?;
    let library = state.library.clone();
    let position = tokio::task::spawn_blocking(move || {
        library.set_position(&id, body.anchor, body.percent, body.expected_revision)
    })
    .await
    .map_err(|_| failure(ReaderError::NotFound))?
    .map_err(failure)?;
    Ok(Json(json!({ "position": position })))
}

#[derive(Deserialize)]
struct MetadataQuery {
    #[serde(default)]
    title: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    isbn: String,
}
#[derive(Deserialize)]
struct MatchSelection {
    work_key: String,
}
#[derive(Deserialize)]
struct MetadataSelection {
    preview_id: Option<String>,
    edits: MetadataEdits,
    #[serde(default = "use_cover_default")]
    use_cover: bool,
}
fn use_cover_default() -> bool {
    true
}
fn lookup_error(message: String) -> Failure {
    (StatusCode::BAD_GATEWAY, Json(json!({"error": message})))
}
fn lookup_busy() -> Failure {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"error":"Another book lookup is running. Please try again shortly."})),
    )
}
async fn search_metadata(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<MetadataQuery>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    state.library.book(&id).map_err(failure)?;
    let Json(query) = body.map_err(|_| {
        failure(ReaderError::Invalid(
            "Enter a title, author or ISBN.".into(),
        ))
    })?;
    let title = query.title.trim();
    let author = query.author.trim();
    let isbn = query.isbn.trim();
    if title.chars().count() > 200
        || author.chars().count() > 160
        || isbn.chars().count() > 32
        || (title.is_empty() && author.is_empty() && isbn.is_empty())
    {
        return Err(failure(ReaderError::Invalid(
            "Enter a title (up to 200 characters), optional author (up to 160) or an ISBN.".into(),
        )));
    }
    if !isbn.is_empty() && normalize_isbn(isbn).is_err() {
        return Err(failure(ReaderError::Invalid(
            "Enter a valid ISBN-10 or ISBN-13.".into(),
        )));
    }
    let _permit = state.lookup_busy.try_acquire().map_err(|_| lookup_busy())?;
    let results = state
        .catalog
        .as_ref()
        .map_err(|e| lookup_error(e.clone()))?
        .search(SearchTerms {
            title,
            author,
            isbn,
        })
        .await
        .map_err(lookup_error)?;
    let mut matches = state.matches.lock().unwrap();
    matches.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(600));
    if matches.len() >= 16 {
        matches.clear();
    }
    matches.insert(id, (Instant::now(), results.clone()));
    Ok(Json(json!({"matches": results})))
}
fn confirmed_match(state: &ReaderState, id: &str, work_key: &str) -> Result<Metadata, Failure> {
    state
        .matches
        .lock()
        .unwrap()
        .get(id)
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(600))
        .and_then(|(_, candidates)| candidates.iter().find(|c| c.work_key == work_key))
        .cloned()
        .ok_or_else(|| {
            failure(ReaderError::Invalid(
                "Search again, then preview one of the returned matches.".into(),
            ))
        })
}
fn cached_preview(state: &ReaderState, id: &str, token: &str) -> Result<MetadataPreview, Failure> {
    let mut previews = state.previews.lock().unwrap();
    previews.retain(|_, p| p.created.elapsed() < Duration::from_secs(600));
    previews.get(id).filter(|p| p.token == token).cloned().ok_or_else(||
        (StatusCode::GONE, Json(json!({"error":"This preview expired. Preview the result again before saving."}))))
}
async fn preview_metadata(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<MatchSelection>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    state.library.book(&id).map_err(failure)?;
    let Json(selection) = body.map_err(|_| {
        failure(ReaderError::Invalid(
            "Choose a search result to preview.".into(),
        ))
    })?;
    let candidate = confirmed_match(&state, &id, &selection.work_key)?;
    let _permit = state.lookup_busy.try_acquire().map_err(|_| lookup_busy())?;
    let (metadata, cover) = state
        .catalog
        .as_ref()
        .map_err(|e| lookup_error(e.clone()))?
        .enrich(candidate)
        .await
        .map_err(lookup_error)?;
    let preview = MetadataPreview {
        token: uuid::Uuid::new_v4().to_string(),
        created: Instant::now(),
        metadata: metadata.clone(),
        cover: cover.map(Arc::new),
    };
    let token = preview.token.clone();
    let mut previews = state.previews.lock().unwrap();
    previews.retain(|_, p| p.created.elapsed() < Duration::from_secs(600));
    if previews.len() >= 16 {
        previews.clear();
    }
    previews.insert(id, preview);
    Ok(Json(json!({"preview_id":token, "metadata":metadata})))
}
async fn preview_cover(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path((id, token)): Path<(String, String)>,
) -> Result<Response, Failure> {
    authorize(&state, &headers)?;
    state.library.book(&id).map_err(failure)?;
    let preview = cached_preview(&state, &id, &token)?;
    let bytes = preview
        .cover
        .ok_or_else(|| failure(ReaderError::NotFound))?;
    Ok((
        [
            ("content-type", "image/jpeg"),
            ("x-content-type-options", "nosniff"),
        ],
        Bytes::copy_from_slice(&bytes),
    )
        .into_response())
}
async fn select_metadata(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<MetadataSelection>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let Json(selection) = body.map_err(|_| {
        failure(ReaderError::Invalid(
            "Preview the details, then save your edits.".into(),
        ))
    })?;
    let library = state.library.clone();
    let preview = selection
        .preview_id
        .as_deref()
        .map(|token| cached_preview(&state, &id, token))
        .transpose()?;
    let book = tokio::task::spawn_blocking(move || {
        let (mut metadata, mut cover) = if let Some(preview) = preview {
            (preview.metadata, preview.cover)
        } else {
            let book = library.book(&id)?;
            let metadata = book
                .metadata
                .ok_or_else(|| ReaderError::Invalid("Preview a matching book first.".into()))?;
            let cover = if metadata.cover_cached && selection.use_cover {
                Some(Arc::new(library.cover(&id)?))
            } else {
                None
            };
            (metadata, cover)
        };
        selection
            .edits
            .apply(&mut metadata)
            .map_err(ReaderError::Invalid)?;
        if !selection.use_cover {
            cover = None;
        }
        metadata.cover_cached = cover.is_some();
        library.set_metadata(&id, Some(metadata), cover.as_deref().map(Vec::as_slice))
    })
    .await
    .map_err(|_| failure(ReaderError::NotFound))?
    .map_err(failure)?;
    Ok(Json(json!({"book": public(&book)})))
}
async fn clear_metadata(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let library = state.library.clone();
    let book = tokio::task::spawn_blocking(move || library.set_metadata(&id, None, None))
        .await
        .map_err(|_| failure(ReaderError::NotFound))?
        .map_err(failure)?;
    Ok(Json(json!({"book": public(&book)})))
}
async fn cover(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    authorize(&state, &headers)?;
    let library = state.library.clone();
    let bytes = tokio::task::spawn_blocking(move || library.cover(&id))
        .await
        .map_err(|_| failure(ReaderError::NotFound))?
        .map_err(failure)?;
    Ok((
        [
            ("content-type", "image/jpeg"),
            ("x-content-type-options", "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

async fn speech_info(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    authorize(&state, &headers)?;
    let mut info = json!({
        "available": state.speech.is_some(),
        "backend": "host",
        "voices": VOICES.as_slice(),
        "default_voice": "af_heart",
        "speed": { "min": MIN_SPEED, "max": MAX_SPEED, "default": 1.0 },
        "max_chars": MAX_SPEECH_CHARS,
    });
    if state.speech.is_none() {
        info["reason"] = json!("The host voice is not running.");
    }
    Ok(Json(info))
}

#[derive(Deserialize)]
struct SpeechBody {
    text: String,
    voice: String,
    #[serde(default = "default_speed")]
    speed: f32,
}

fn default_speed() -> f32 {
    1.0
}

async fn speak(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    body: Result<Json<SpeechBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Failure> {
    authorize(&state, &headers)?;
    let bad = |message: &str| (StatusCode::BAD_REQUEST, Json(json!({ "error": message })));
    let Json(body) = body.map_err(|_| bad("Send { text, voice, speed }."))?;
    let chars = body.text.chars().count();
    if body.text.trim().is_empty() || chars > MAX_SPEECH_CHARS {
        return Err(bad("Send 1 to 600 characters of text."));
    }
    if !VOICES.contains(&body.voice.as_str()) {
        return Err(bad("Unknown voice."));
    }
    if !(MIN_SPEED..=MAX_SPEED).contains(&body.speed) {
        return Err(bad("Speed must be between 0.7 and 1.5."));
    }
    let speech = state.speech.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "The host voice is not running."})),
        )
    })?;
    let _turn = state.speaking.try_acquire().map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "The host voice is reading another sentence."})),
        )
    })?;
    let started = std::time::Instant::now();
    let (pcm, rate) = speech
        .speak(
            &body.text,
            &body.voice,
            voice_language(&body.voice),
            body.speed,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, chars, "Reader host speech failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "The host voice could not read this sentence."})),
            )
        })?;
    let elapsed = started.elapsed().as_millis();
    tracing::debug!(
        chars,
        elapsed_ms = elapsed,
        samples = pcm.len(),
        "Reader host speech"
    );
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/octet-stream".to_string(),
            ),
            (
                axum::http::HeaderName::from_static("x-sample-rate"),
                rate.to_string(),
            ),
            (
                axum::http::HeaderName::from_static("x-synthesis-ms"),
                elapsed.to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn remove(
    State(state): State<Arc<ReaderState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    authorize(&state, &headers)?;
    let library = state.library.clone();
    tokio::task::spawn_blocking(move || library.remove(&id))
        .await
        .map_err(|_| failure(ReaderError::NotFound))?
        .map_err(failure)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn app() -> (Router, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sail-reader-http-{}", uuid::Uuid::new_v4()));
        let library = Arc::new(Library::open(&dir).unwrap());
        (router(library, ApprovalAuthority::new(TOKEN).ok()), dir)
    }

    async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn request(method: &str, uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {TOKEN}"))
    }

    #[tokio::test]
    async fn preview_and_edit_are_separate_and_cover_bytes_are_preserved() {
        let dir = std::env::temp_dir().join(format!("sail-preview-{}", uuid::Uuid::new_v4()));
        let library = Arc::new(Library::open(&dir).unwrap());
        let imported = library
            .import("original.txt", b"A book to read.")
            .unwrap()
            .book;
        let metadata: Metadata = serde_json::from_value(json!({
            "work_key":"/works/OL1W", "title":"Provider title", "authors":["Provider author"],
            "first_publish_year":1900, "edition_count":1, "subjects":["Fiction"], "description":"Description",
            "cover_id":1, "cover_cached":true, "retrieved_at":chrono::Utc::now()
        })).unwrap();
        let state = Arc::new(ReaderState {
            library: library.clone(),
            authority: ApprovalAuthority::new(TOKEN).ok(),
            speech: None,
            speaking: tokio::sync::Semaphore::new(1),
            catalog: Catalog::new(),
            lookup_busy: tokio::sync::Semaphore::new(1),
            matches: Mutex::new(HashMap::new()),
            previews: Mutex::new(HashMap::new()),
        });
        state.previews.lock().unwrap().insert(
            imported.id.clone(),
            MetadataPreview {
                token: "preview-one".into(),
                created: Instant::now(),
                metadata,
                cover: Some(Arc::new(b"preview jpeg".to_vec())),
            },
        );
        let app = router_with_state(state.clone());
        let path = format!("/books/{}/metadata", imported.id);
        let image_path = format!("{path}/preview/preview-one/cover");
        let (status, _) = call(&app, Request::get(&image_path).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(request("GET", &image_path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .as_ref(),
            b"preview jpeg"
        );
        assert!(library.book(&imported.id).unwrap().metadata.is_none());
        assert!(library.cover(&imported.id).is_err());
        let mut edits = json!({"title":"My title", "authors":["My author"], "first_publish_year":1999,
            "subjects":["Custom"],"description":"My description"});
        let save = |preview: Value, edits: Value, use_cover: bool| {
            request("POST", &path)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"preview_id":preview,"edits":edits,"use_cover":use_cover}).to_string(),
                ))
                .unwrap()
        };
        let mut invalid = edits.clone();
        invalid["title"] = json!(" ");
        let (status, _) = call(&app, save(json!("preview-one"), invalid, true)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(library.book(&imported.id).unwrap().metadata.is_none());
        let (status, saved) = call(&app, save(json!("preview-one"), edits.clone(), true)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["book"]["metadata"]["title"], "My title");
        assert_eq!(saved["book"]["metadata"]["user_edited"], true);
        assert_eq!(library.cover(&imported.id).unwrap(), b"preview jpeg");
        assert_eq!(library.book(&imported.id).unwrap().title, imported.title);
        edits["description"] = json!("Offline correction");
        let (status, saved) = call(&app, save(Value::Null, edits, false)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            saved["book"]["metadata"]["description"],
            "Offline correction"
        );
        assert!(library.cover(&imported.id).is_err());
        state
            .previews
            .lock()
            .unwrap()
            .get_mut(&imported.id)
            .unwrap()
            .created = Instant::now() - Duration::from_secs(601);
        let (status, _) = call(
            &app,
            request("GET", &image_path).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(
            Library::open(&dir)
                .unwrap()
                .book(&imported.id)
                .unwrap()
                .metadata
                .unwrap()
                .description
                .as_deref(),
            Some("Offline correction")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Explicit smoke test against the real service, never part of offline CI.
    #[tokio::test]
    #[ignore = "uses the real Open Library service"]
    async fn live_open_library_lookup_roundtrip() {
        let (app, dir) = app();
        let (_, imported) = call(
            &app,
            request("POST", "/import")
                .header("x-filename", "The Time Machine.txt")
                .body(Body::from(
                    "Chapter one\n\nThe Time Traveller was expounding a recondite matter to us.",
                ))
                .unwrap(),
        )
        .await;
        let id = imported["book"]["id"].as_str().unwrap();
        let (status, result) = call(
            &app,
            request("POST", &format!("/books/{id}/metadata/search"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"title":"The Time Machine","author":"H. G. Wells"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let candidate = &result["matches"][0];
        assert_eq!(candidate["title"], "The Time Machine");
        let (status, result) = call(
            &app,
            request("POST", &format!("/books/{id}/metadata/preview"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"work_key":candidate["work_key"]}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["metadata"]["cover_cached"], true, "{result}");
        let (_, listed) = call(
            &app,
            request("GET", "/library").body(Body::empty()).unwrap(),
        )
        .await;
        assert!(listed["books"][0]["metadata"].is_null());
        let (status, result) = call(&app, request("POST", &format!("/books/{id}/metadata"))
            .header("content-type", "application/json").body(Body::from(json!({
                "preview_id":result["preview_id"], "edits":result["metadata"], "use_cover":true
            }).to_string())).unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["book"]["metadata"]["cover_cached"], true, "{result}");
        let response = app
            .oneshot(
                request("GET", &format!("/books/{id}/cover"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        assert!(image::load_from_memory(&bytes).is_ok());
        let cached = Library::open(&dir).unwrap();
        assert_eq!(cached.cover(id).unwrap(), bytes.as_ref());
        assert!(cached.book(id).unwrap().metadata.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn metadata_and_cover_require_owner_and_confirmed_search_match() {
        let (app, dir) = app();
        let (_, body) = call(
            &app,
            request("POST", "/import")
                .header("x-filename", "sample.txt")
                .body(Body::from("A sample book."))
                .unwrap(),
        )
        .await;
        let id = body["book"]["id"].as_str().unwrap();
        for (method, suffix) in [
            ("POST", "metadata/search"),
            ("POST", "metadata/preview"),
            ("GET", "metadata/preview/unknown/cover"),
            ("POST", "metadata"),
            ("DELETE", "metadata"),
            ("GET", "cover"),
        ] {
            let path = format!("/books/{id}/{suffix}");
            let (status, _) = call(
                &app,
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        let (status, _) = call(
            &app,
            request("POST", &format!("/books/{id}/metadata"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"work_key":"/works/OL1W"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "unconfirmed matches never trigger a network request"
        );
        let (status, _) = call(
            &app,
            request("POST", &format!("/books/{id}/metadata/search"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"","author":""}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, body) = call(
            &app,
            request("POST", &format!("/books/{id}/metadata/search"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"","author":"","isbn":"123"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"].as_str().unwrap_or_default().contains("ISBN"),
            "{body}"
        );
        let (status, _) = call(
            &app,
            request("GET", &format!("/books/{id}/cover"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // Cached art is available only through the same owner-authenticated local route.
        let library = Library::open(&dir).unwrap();
        library
            .set_metadata(
                id,
                Some(Metadata {
                    work_key: "/works/OL1W".into(),
                    title: "A sample".into(),
                    authors: vec![],
                    first_publish_year: None,
                    edition_count: None,
                    subjects: vec![],
                    description: None,
                    cover_id: Some(1),
                    cover_olid: None,
                    subtitle: None,
                    publishers: Vec::new(),
                    languages: Vec::new(),
                    isbn: Vec::new(),
                    cover_cached: true,
                    retrieved_at: chrono::Utc::now(),
                    user_edited: false,
                }),
                Some(b"cached jpeg"),
            )
            .unwrap();
        let cached_app = router(Arc::new(library), ApprovalAuthority::new(TOKEN).ok());
        let response = cached_app
            .clone()
            .oneshot(
                request("GET", &format!("/books/{id}/cover"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "image/jpeg");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .as_ref(),
            b"cached jpeg"
        );
        let (status, body) = call(
            &cached_app,
            request("DELETE", &format!("/books/{id}/metadata"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["book"]["metadata"].is_null());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn owner_imports_reads_and_saves_positions() {
        let (app, dir) = app();
        let (status, _) = call(&app, Request::get("/library").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let txt =
            "Chapter 1\n\nFirst paragraph.\n\nChapter 2\n\nSecond <script>alert(1)</script>.\n";
        let upload = || {
            request("POST", "/import")
                .header("x-filename", "fixture.txt")
                .body(Body::from(txt))
                .unwrap()
        };
        let (status, body) = call(&app, upload()).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["book"]["id"].as_str().unwrap().to_string();
        assert_eq!(body["book"]["sections"], 2);
        assert!(body["book"].get("section_info").is_none());
        let (status, again) = call(&app, upload()).await;
        assert_eq!(
            (status, again["book"]["id"].as_str()),
            (StatusCode::OK, Some(id.as_str()))
        );

        let (status, detail) = call(
            &app,
            request("GET", &format!("/books/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["toc"][1]["title"], "Chapter 2");
        assert_eq!(detail["sections"][1]["blocks"], 2);

        let (_, section) = call(
            &app,
            request("GET", &format!("/books/{id}/sections/1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        // Book text is returned as plain text for the client to render with textContent.
        assert_eq!(
            section["blocks"][1],
            json!({"kind":"p","text":"Second <script>alert(1)</script>."})
        );

        let put = |revision: u64| {
            request("PUT", &format!("/books/{id}/position"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"anchor":{"section":1,"block":1,"char":3},"percent":60.0,"expected_revision":revision}).to_string()))
                .unwrap()
        };
        let (status, saved) = call(&app, put(0)).await;
        assert_eq!(
            (status, saved["position"]["revision"].as_u64()),
            (StatusCode::OK, Some(1))
        );
        let (status, stale) = call(&app, put(0)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale["position"]["revision"], 1);
        let (_, listed) = call(
            &app,
            request("GET", "/library").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(listed["books"][0]["position"]["anchor"]["block"], 1);

        let (status, _) = call(
            &app,
            request("DELETE", &format!("/books/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(
            &app,
            request("GET", &format!("/books/{id}/position"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(dir);
    }

    struct SlowNarrator(std::sync::Mutex<Vec<(String, String, String, f32)>>);

    #[async_trait::async_trait]
    impl Narrator for SlowNarrator {
        async fn speak(
            &self,
            text: &str,
            voice: &str,
            language: &str,
            speed: f32,
        ) -> Result<(Vec<i16>, u32), String> {
            self.0
                .lock()
                .unwrap()
                .push((text.into(), voice.into(), language.into(), speed));
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            if text == "fail" {
                return Err("worker died".into());
            }
            Ok((vec![1, -2, 300], 24_000))
        }
    }

    #[tokio::test]
    async fn host_speech_reads_text_literally_one_at_a_time() {
        let dir = std::env::temp_dir().join(format!("sail-reader-speech-{}", uuid::Uuid::new_v4()));
        let narrator = Arc::new(SlowNarrator(Default::default()));
        let app = router_with_speech(
            Arc::new(Library::open(&dir).unwrap()),
            ApprovalAuthority::new(TOKEN).ok(),
            Some(narrator.clone()),
        );
        let (status, info) =
            call(&app, request("GET", "/speech").body(Body::empty()).unwrap()).await;
        assert_eq!(
            (status, info["available"].as_bool()),
            (StatusCode::OK, Some(true))
        );
        assert_eq!(info["voices"].as_array().unwrap().len(), 54);
        let say = |body: Value| {
            request("POST", "/speech")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(say(
                json!({"text":"Hello there.","voice":"bf_emma","speed":1.25}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-sample-rate"], "24000");
        assert!(response.headers().contains_key("x-synthesis-ms"));
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&bytes[..], &[1, 0, 0xfe, 0xff, 0x2c, 0x01]);
        assert_eq!(
            narrator.0.lock().unwrap()[0],
            (
                "Hello there.".into(),
                "bf_emma".into(),
                "en-gb".into(),
                1.25
            )
        );

        // A second request while one is being read is refused, not queued.
        let first = tokio::spawn(
            app.clone()
                .oneshot(say(json!({"text":"One.","voice":"af_heart"}))),
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let (status, _) = call(&app, say(json!({"text":"Two.","voice":"af_heart"}))).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(first.await.unwrap().unwrap().status(), StatusCode::OK);

        for (body, expected) in [
            (
                json!({"text":"","voice":"af_heart"}),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({"text":"x".repeat(601),"voice":"af_heart"}),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({"text":"Hi.","voice":"../../etc"}),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({"text":"Hi.","voice":"af_heart","speed":3.0}),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({"text":"fail","voice":"af_heart"}),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ] {
            assert_eq!(call(&app, say(body.clone())).await.0, expected, "{body}");
        }

        let (app_without, dir2) = self::app();
        let (_, info) = call(
            &app_without,
            request("GET", "/speech").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(info["available"], false);
        let (status, _) = call(&app_without, say(json!({"text":"Hi.","voice":"af_heart"}))).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[tokio::test]
    async fn refuses_unsupported_unnamed_and_malformed_requests() {
        let (app, dir) = app();
        let (status, body) = call(
            &app,
            request("POST", "/import")
                .header("x-filename", "a.pdf")
                .body(Body::from("%PDF"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(body["error"].as_str().unwrap().contains(".epub"));
        let (status, _) = call(
            &app,
            request("POST", "/import").body(Body::from("x")).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app,
            request("POST", "/import")
                .header("x-filename", "a.epub")
                .body(Body::from("not a zip"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app,
            request("GET", "/books/../../etc/sections/0")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert!(status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn metadata_dialog_exposes_isbn_and_sends_it() {
        let html = include_str!("../web/reader.html");
        assert!(
            html.contains("id=\"metadata-isbn\""),
            "ISBN input is missing"
        );
        assert!(
            html.contains("optional ISBN"),
            "search privacy text is stale"
        );
        let js = include_str!("../web/reader.js");
        assert!(
            js.contains("body.isbn = isbn"),
            "the dialog no longer sends the optional ISBN"
        );
    }
}
