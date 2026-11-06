//! SAIL Reader's owner API, nested at `/api/reader` (issue #333).
//!
//! Every route needs the owner bearer token. Books arrive as raw bytes with their name in
//! `X-Filename` and are parsed off the async runtime. Responses carry only plain-text blocks,
//! never book HTML. Nothing here reaches Mind or any model: reading is deterministic.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sail_bridge::reader::{Anchor, Book, Library, ReaderError};
use sail_guardian::authority::ApprovalAuthority;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

struct ReaderState {
    library: Arc<Library>,
    authority: Option<ApprovalAuthority>,
}

type Failure = (StatusCode, Json<Value>);

/// The reader routes, or `None` (logged) when the library cannot be opened.
pub fn routes(authority: Option<ApprovalAuthority>) -> Option<Router> {
    match sail_bridge::reader::global() {
        Ok(library) => Some(router(library, authority)),
        Err(error) => {
            tracing::warn!(%error, "Reader library unavailable; reader disabled");
            None
        }
    }
}

pub fn router(library: Arc<Library>, authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/library", get(library_list))
        .route("/import", post(import))
        .route("/books/:id", get(book).delete(remove))
        .route("/books/:id/sections/:index", get(section))
        .route("/books/:id/position", get(position).put(save_position))
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
        .with_state(Arc::new(ReaderState { library, authority }))
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
}
