//! Owner-initiated Open Library lookup. Only title/author leave the host, never book text.
//! Fixed HTTPS endpoints, restricted cover-storage redirects, bounded bodies and rate limits.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

const MAX_JSON: usize = 512 * 1024;
pub const MAX_COVER: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub work_key: String,
    pub title: String,
    pub authors: Vec<String>,
    pub first_publish_year: Option<i64>,
    pub edition_count: Option<u64>,
    pub subjects: Vec<String>,
    pub description: Option<String>,
    pub cover_id: Option<u64>,
    pub cover_cached: bool,
    pub retrieved_at: DateTime<Utc>,
}

impl Metadata {
    pub fn source_url(&self) -> String {
        format!("https://openlibrary.org{}", self.work_key)
    }
}

pub struct Catalog {
    client: reqwest::Client,
    last_request: tokio::sync::Mutex<Option<Instant>>,
    api: String,
    covers: String,
}

impl Catalog {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .user_agent("SAIL-Reader/0.1 (personal local-first library; https://github.com/PedroVictorCoding/SAIL)")
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let cover_request = attempt.previous().first().is_some_and(|u| u.host_str() == Some("covers.openlibrary.org"));
                if cover_request && attempt.previous().len() <= 3 && allowed_cover_redirect(attempt.url()) {
                    attempt.follow()
                } else { attempt.stop() }
            }))
            .timeout(Duration::from_secs(12))
            .connect_timeout(Duration::from_secs(4))
            .build().map_err(|_| "Book lookup could not start.".to_string())?;
        Ok(Self {
            client,
            last_request: tokio::sync::Mutex::new(None),
            api: "https://openlibrary.org".into(),
            covers: "https://covers.openlibrary.org".into(),
        })
    }

    async fn bytes(&self, url: reqwest::Url, max: usize) -> Result<Vec<u8>, String> {
        let mut last = self.last_request.lock().await;
        if let Some(at) = *last {
            tokio::time::sleep(Duration::from_millis(1100).saturating_sub(at.elapsed())).await;
        }
        *last = Some(Instant::now());
        let mut response = self.client.get(url).send().await.map_err(|_| {
            "Open Library could not be reached. You can keep reading offline.".to_string()
        })?;
        if !response.status().is_success() {
            return Err(
                "Open Library is unavailable or rate limited. Please try again later.".into(),
            );
        }
        if response.content_length().is_some_and(|n| n > max as u64) {
            return Err("Open Library returned a response that is too large.".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Book lookup was interrupted.".to_string())?
        {
            if bytes.len() + chunk.len() > max {
                return Err("Open Library returned a response that is too large.".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub async fn search(&self, title: &str, author: &str) -> Result<Vec<Metadata>, String> {
        if title.trim().is_empty() || title.chars().count() > 200 || author.chars().count() > 160 {
            return Err(
                "Enter a title (up to 200 characters) and optional author (up to 160 characters)."
                    .into(),
            );
        }
        let mut url = reqwest::Url::parse(&format!("{}/search.json", self.api)).unwrap();
        url.query_pairs_mut()
            .append_pair("title", title.trim())
            .append_pair(
                "fields",
                "key,title,author_name,first_publish_year,edition_count,cover_i,subject",
            )
            .append_pair("limit", "5");
        if !author.trim().is_empty() {
            url.query_pairs_mut().append_pair("author", author.trim());
        }
        let data: Value = serde_json::from_slice(&self.bytes(url, MAX_JSON).await?)
            .map_err(|_| "Open Library returned unreadable book details.".to_string())?;
        let docs = data["docs"]
            .as_array()
            .ok_or("Open Library returned unreadable book details.")?;
        Ok(docs.iter().take(5).filter_map(candidate).collect())
    }

    pub async fn enrich(&self, mut book: Metadata) -> Result<(Metadata, Option<Vec<u8>>), String> {
        if !valid_work_key(&book.work_key) {
            return Err("Invalid Open Library work.".into());
        }
        // Descriptions and covers are best effort: a missing image must not discard metadata.
        let url = reqwest::Url::parse(&format!("{}{}.json", self.api, book.work_key)).unwrap();
        if let Ok(bytes) = self.bytes(url, MAX_JSON).await
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            book.description = text(
                value
                    .get("description")
                    .and_then(|v| v.as_str().or_else(|| v["value"].as_str())),
                1600,
            );
        }
        let mut image = None;
        if let Some(id) = book.cover_id {
            let url =
                reqwest::Url::parse(&format!("{}/b/id/{id}-L.jpg?default=false", self.covers))
                    .unwrap();
            if let Ok(bytes) = self.bytes(url, MAX_COVER).await {
                image = tokio::task::spawn_blocking(move || sanitize_cover(&bytes))
                    .await
                    .ok()
                    .and_then(Result::ok);
            }
        }
        book.cover_cached = image.is_some();
        book.retrieved_at = Utc::now();
        Ok((book, image))
    }
}

// Open Library stores large covers in l_covers archives. No arbitrary host, port or
// redirected metadata URL is accepted, and owner bearer tokens are never in this client.
fn allowed_cover_redirect(url: &reqwest::Url) -> bool {
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let host = url.host_str().unwrap_or_default();
    if host == "covers.openlibrary.org" {
        return url.path().starts_with("/b/id/");
    }
    if host == "archive.org" {
        return url.path().starts_with("/download/l_covers_");
    }
    let archive_server = [".us.archive.org", ".eu.archive.org"].iter().any(|suffix| {
        host.strip_suffix(suffix)
            .and_then(|h| h.strip_prefix("ia"))
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()))
    });
    archive_server && url.path() == "/view_archive.php"
}

pub fn valid_work_key(value: &str) -> bool {
    value
        .strip_prefix("/works/OL")
        .and_then(|v| v.strip_suffix('W'))
        .is_some_and(|n| !n.is_empty() && n.len() <= 16 && n.bytes().all(|c| c.is_ascii_digit()))
}
fn text(value: Option<&str>, limit: usize) -> Option<String> {
    value
        .map(|s| s.trim().chars().take(limit).collect::<String>())
        .filter(|s| !s.is_empty())
}
fn strings(value: &Value, count: usize, limit: usize) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| text(v.as_str(), limit))
        .take(count)
        .collect()
}
fn candidate(doc: &Value) -> Option<Metadata> {
    let key = doc["key"].as_str()?;
    if !valid_work_key(key) {
        return None;
    }
    Some(Metadata {
        work_key: key.into(),
        title: text(doc["title"].as_str(), 300)?,
        authors: strings(&doc["author_name"], 8, 160),
        first_publish_year: doc["first_publish_year"].as_i64(),
        edition_count: doc["edition_count"].as_u64(),
        subjects: strings(&doc["subject"], 4, 80),
        description: None,
        cover_id: doc["cover_i"].as_u64().filter(|id| *id > 0),
        cover_cached: false,
        retrieved_at: Utc::now(),
    })
}

/// Re-encode a bounded raster as JPEG; never store SVG, metadata or arbitrary provider bytes.
fn sanitize_cover(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Cursor;
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Invalid cover")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| "Invalid cover")?;
    let image = if image.width() > 600 || image.height() > 900 {
        image.thumbnail(600, 900)
    } else {
        image
    };
    let image = image.to_rgb8();
    let mut output = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 85)
        .encode_image(&image)
        .map_err(|_| "Invalid cover")?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn redirects_are_limited_to_https_cover_storage() {
        for url in [
            "https://archive.org/download/l_covers_0009/file.jpg",
            "https://ia600507.us.archive.org/view_archive.php?file=cover.jpg",
        ] {
            assert!(allowed_cover_redirect(&reqwest::Url::parse(url).unwrap()));
        }
        for url in [
            "http://archive.org/download/l_covers_0009",
            "https://archive.org:8443/download/l_covers_1",
            "https://archive.org/download/private",
            "https://ia600507.us.archive.org.evil.test/view_archive.php",
            "https://evil.us.archive.org/view_archive.php",
            "https://user:secret@archive.org/download/l_covers_1",
            "https://127.0.0.1/cover.jpg",
        ] {
            assert!(
                !allowed_cover_redirect(&reqwest::Url::parse(url).unwrap()),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn oversized_json_and_redirects_are_rejected() {
        use axum::{Router, response::Redirect, routing::get};
        let app = Router::new()
            .route("/large", get(|| async { vec![b'x'; MAX_JSON + 1] }))
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("https://example.com/untrusted") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let catalog = Catalog::new().unwrap();
        assert!(
            catalog
                .bytes(
                    reqwest::Url::parse(&format!("{base}/large")).unwrap(),
                    MAX_JSON
                )
                .await
                .unwrap_err()
                .contains("too large")
        );
        assert!(
            catalog
                .bytes(
                    reqwest::Url::parse(&format!("{base}/redirect")).unwrap(),
                    MAX_JSON
                )
                .await
                .is_err()
        );
        server.abort();
    }

    #[test]
    fn candidate_rejects_paths_and_bounds_untrusted_fields() {
        for key in [
            "/works/../../evil",
            "https://evil.test",
            "/works/OL1W?x=1",
            "/works/OLW",
        ] {
            assert!(!valid_work_key(key));
        }
        let metadata = candidate(&json!({"key":"/works/OL123W","title":"X".repeat(500),"author_name":["A"],"cover_i":123})).unwrap();
        assert_eq!(metadata.title.len(), 300);
        assert!(!metadata.cover_cached);
        assert_eq!(
            metadata.source_url(),
            "https://openlibrary.org/works/OL123W"
        );
        assert!(candidate(&json!({"key":"https://evil.test","title":"X"})).is_none());
    }
    #[test]
    fn cover_is_reencoded_and_non_raster_is_rejected() {
        assert!(sanitize_cover(b"<svg onload='evil()'/>").is_err());
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut bytes)
            .encode_image(&image::RgbImage::new(20, 30))
            .unwrap();
        let cover = sanitize_cover(&bytes).unwrap();
        assert_eq!(image::load_from_memory(&cover).unwrap().width(), 20);
    }
    #[tokio::test]
    async fn lookup_uses_fixed_paths_and_query_encoding() {
        use axum::{Json, Router, extract::Query, routing::get};
        let app = Router::new()
            .route(
                "/search.json",
                get(
                    |Query(q): Query<std::collections::HashMap<String, String>>| async move {
                        assert_eq!(q["title"], "A & B");
                        assert_eq!(q["author"], "One? Two");
                        assert_eq!(q["limit"], "5");
                        Json(json!({"docs":[{"key":"/works/OL1W","title":"A & B","cover_i":5}]}))
                    },
                ),
            )
            .route(
                "/works/OL1W.json",
                get(|| async { Json(json!({"description":{"value":"A description"}})) }),
            )
            .route(
                "/b/id/5-L.jpg",
                get(|| async { (axum::http::StatusCode::NOT_FOUND, "missing") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut catalog = Catalog::new().unwrap();
        catalog.api = base.clone();
        catalog.covers = base;
        let results = catalog.search("A & B", "One? Two").await.unwrap();
        let (metadata, cover) = catalog.enrich(results[0].clone()).await.unwrap();
        assert_eq!(metadata.description.as_deref(), Some("A description"));
        assert!(cover.is_none());
        assert!(catalog.search("", "").await.is_err());
        server.abort();
    }
}
