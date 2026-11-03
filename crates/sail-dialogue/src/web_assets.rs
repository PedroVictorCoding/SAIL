//! Serves the in-browser speech chain (`scripts/voice/download_web.py`) from SAIL's own
//! origin, so the browser runs VAD, Whisper and Kokoro without contacting the internet.
//!
//! Only files named in the installed `manifest.json` are served, each with its pinned
//! SHA-256 as the ETag. Paths outside the manifest, symlinks and anything that is not a
//! regular file are refused. The manifest is written last by the download script, so a
//! partial download is never offered; clients then keep using the host speech workers.

use axum::{
    Router,
    body::Body,
    extract::Path as UrlPath,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

pub const MANIFEST: &str = "manifest.json";

/// `SAIL_VOICE_WEB_DIR`, else `$SAIL_RUNTIME/models/voice/web` (runtime SSD).
pub fn assets_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SAIL_VOICE_WEB_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("SAIL_RUNTIME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/srv/sail"))
        .join("models/voice/web")
}

/// Routes under `/voice-assets`, serving from `root`.
pub fn router(root: PathBuf) -> Router {
    let manifest_root = root.clone();
    Router::new()
        .route(
            "/manifest.json",
            get(move || serve_manifest(manifest_root.clone())),
        )
        .route(
            "/*path",
            get(move |UrlPath(path): UrlPath<String>, headers: HeaderMap| {
                serve(root.clone(), path, headers)
            }),
        )
}

#[derive(Deserialize)]
struct Manifest {
    files: HashMap<String, ManifestFile>,
}
#[derive(Deserialize)]
struct ManifestFile {
    sha256: String,
}

fn read_manifest(root: &Path) -> Option<Manifest> {
    let bytes = std::fs::read(regular_file(root, Path::new(MANIFEST))?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `root/rel` if every component below `root` is a plain directory or, last, a regular
/// file: no `..`, absolute paths or symlinks.
fn regular_file(root: &Path, rel: &Path) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    let mut components = rel.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(part) = component else {
            return None;
        };
        path.push(part);
        let meta = std::fs::symlink_metadata(&path).ok()?;
        let ok = if components.peek().is_some() {
            meta.is_dir()
        } else {
            meta.is_file()
        };
        if !ok {
            return None;
        }
    }
    (path != root).then_some(path)
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("js" | "mjs") => "text/javascript",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

async fn serve_manifest(root: PathBuf) -> Response {
    let Some(path) = regular_file(&root, Path::new(MANIFEST)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve(root: PathBuf, rel: String, headers: HeaderMap) -> Response {
    let found = tokio::task::spawn_blocking(move || {
        let manifest = read_manifest(&root)?;
        let sha = manifest.files.get(&rel)?.sha256.clone();
        Some((regular_file(&root, Path::new(&rel))?, sha, rel))
    })
    .await
    .ok()
    .flatten();
    let Some((path, sha, rel)) = found else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!("\"{sha}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| v.as_bytes() == etag.as_bytes())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let len = file.metadata().await.map(|m| m.len()).ok();
    let mut response =
        Body::from_stream(tokio_util::io::ReaderStream::with_capacity(file, 1 << 16))
            .into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(&rel)),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    if let Some(len) = len {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("voice-web-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("lib/a.js"), "export const a = 1;").unwrap();
        std::fs::write(root.join("lib/unlisted.js"), "secret").unwrap();
        std::fs::write(
            root.join(MANIFEST),
            r#"{"version":1,"files":{"lib/a.js":{"sha256":"abc"},"lib/link.js":{"sha256":"def"}}}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink("/etc/hostname", root.join("lib/link.js")).unwrap();
        root
    }

    async fn get(root: &Path, path: &str, etag: Option<&str>) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(e) = etag {
            headers.insert(header::IF_NONE_MATCH, HeaderValue::from_str(e).unwrap());
        }
        serve(root.to_path_buf(), path.into(), headers).await
    }

    #[tokio::test]
    async fn serves_only_manifest_files_with_type_and_etag() {
        let root = fixture();
        let ok = get(&root, "lib/a.js", None).await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(ok.headers()[header::CONTENT_TYPE], "text/javascript");
        assert_eq!(ok.headers()[header::ETAG], "\"abc\"");
        let body = axum::body::to_bytes(ok.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"export const a = 1;");

        assert_eq!(
            get(&root, "lib/a.js", Some("\"abc\"")).await.status(),
            StatusCode::NOT_MODIFIED
        );
        for refused in [
            "lib/unlisted.js",
            "lib/link.js",
            "../etc/passwd",
            "lib/../manifest.json",
        ] {
            assert_eq!(
                get(&root, refused, None).await.status(),
                StatusCode::NOT_FOUND,
                "{refused} was served"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn nothing_is_served_before_the_manifest_exists() {
        let root = fixture();
        std::fs::remove_file(root.join(MANIFEST)).unwrap();
        assert_eq!(
            get(&root, "lib/a.js", None).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            serve_manifest(root.clone()).await.status(),
            StatusCode::NOT_FOUND
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn content_types_for_runtime_files() {
        assert_eq!(content_type("lib/ort.jsep.mjs"), "text/javascript");
        assert_eq!(content_type("lib/ort.jsep.wasm"), "application/wasm");
        assert_eq!(
            content_type("hf/x/onnx/model.onnx"),
            "application/octet-stream"
        );
    }
}
