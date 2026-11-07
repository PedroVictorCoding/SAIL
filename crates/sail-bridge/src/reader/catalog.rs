//! Owner-initiated Open Library lookup. Only title/author leave the host, never book text.
//! Fixed HTTPS endpoints, restricted cover-storage redirects, bounded bodies and rate limits.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

const MAX_JSON: usize = 512 * 1024;
pub const MAX_COVER: usize = 2 * 1024 * 1024;
const SEARCH_FIELDS: &str = "key,title,subtitle,author_name,first_publish_year,edition_count,cover_i,cover_edition_key,subject,language,publisher,isbn";
const SEARCH_FETCH: &str = "12";
const SEARCH_RETURN: usize = 5;
const DERIVATIVE_WORDS: [&str; 10] = [
    "summary",
    "study guide",
    "analysis",
    "collection",
    "box set",
    "books set",
    "trilogy",
    "omnibus",
    "adaptation",
    "companion",
];

/// The owner-visible title, author and optional ISBN. Only these leave the host.
#[derive(Debug, Clone, Copy)]
pub struct SearchTerms<'a> {
    pub title: &'a str,
    pub author: &'a str,
    pub isbn: &'a str,
}

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
    /// Edition OLID used when the work has no `cover_i`; validated before a cover request.
    #[serde(default)]
    pub cover_olid: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub isbn: Vec<String>,
    pub cover_cached: bool,
    pub retrieved_at: DateTime<Utc>,
    #[serde(default)]
    pub user_edited: bool,
}

impl Metadata {
    pub fn source_url(&self) -> String {
        format!("https://openlibrary.org{}", self.work_key)
    }
}

/// Local editable fields. Provider identity and cover URLs are deliberately not editable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEdits {
    pub title: String,
    pub authors: Vec<String>,
    pub first_publish_year: Option<i64>,
    pub subjects: Vec<String>,
    pub description: Option<String>,
}
impl MetadataEdits {
    pub fn apply(&self, metadata: &mut Metadata) -> Result<(), String> {
        if self.title.trim().is_empty()
            || self.title.chars().count() > 300
            || self.authors.len() > 8
            || self.authors.iter().any(|s| s.chars().count() > 160)
            || self.subjects.len() > 4
            || self.subjects.iter().any(|s| s.chars().count() > 80)
            || self
                .description
                .as_ref()
                .is_some_and(|s| s.chars().count() > 1600)
            || self
                .first_publish_year
                .is_some_and(|y| !(-9999..=9999).contains(&y))
        {
            return Err("Check the title, authors, year, subjects and description lengths.".into());
        }
        let title = self.title.trim().to_string();
        let authors: Vec<String> = self
            .authors
            .iter()
            .filter_map(|s| text(Some(s), 160))
            .collect();
        let subjects: Vec<String> = self
            .subjects
            .iter()
            .filter_map(|s| text(Some(s), 80))
            .collect();
        let description = text(self.description.as_deref(), 1600);
        metadata.user_edited |= metadata.title != title
            || metadata.authors != authors
            || metadata.first_publish_year != self.first_publish_year
            || metadata.subjects != subjects
            || metadata.description != description;
        metadata.title = title;
        metadata.authors = authors;
        metadata.first_publish_year = self.first_publish_year;
        metadata.subjects = subjects;
        metadata.description = description;
        Ok(())
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

    async fn search_docs(&self, params: &[(&str, &str)]) -> Result<Vec<Metadata>, String> {
        let mut url = reqwest::Url::parse(&format!("{}/search.json", self.api)).unwrap();
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key, value);
            }
            query
                .append_pair("fields", SEARCH_FIELDS)
                .append_pair("limit", SEARCH_FETCH);
        }
        let data: Value = serde_json::from_slice(&self.bytes(url, MAX_JSON).await?)
            .map_err(|_| "Open Library returned unreadable book details.".to_string())?;
        let docs = data["docs"]
            .as_array()
            .ok_or("Open Library returned unreadable book details.")?;
        Ok(docs.iter().filter_map(candidate).collect())
    }

    /// Search the documented Open Library API. Exact title/author fields are tried first;
    /// when they only return box sets or no close match, a ranked general query is merged in.
    /// An ISBN is an exact lookup and never falls back to a fuzzy title search.
    pub async fn search(&self, terms: SearchTerms<'_>) -> Result<Vec<Metadata>, String> {
        let title = terms.title.trim();
        let author = terms.author.trim();
        let isbn_input = terms.isbn.trim();
        if title.chars().count() > 200
            || author.chars().count() > 160
            || isbn_input.chars().count() > 32
        {
            return Err(
                "Enter a title (up to 200 characters), optional author (up to 160) and optional ISBN."
                    .into(),
            );
        }
        let isbn = if isbn_input.is_empty() {
            None
        } else {
            Some(normalize_isbn(isbn_input)?)
        };
        if title.is_empty() && author.is_empty() && isbn.is_none() {
            return Err("Enter a title, author or ISBN to search Open Library.".into());
        }
        if let Some(isbn) = isbn {
            let q = format!("isbn:{isbn}");
            let docs = self.search_docs(&[("q", q.as_str())]).await?;
            return Ok(rank_and_limit(docs, title, author));
        }
        if title.is_empty() {
            let q = format!("author:(\"{}\")", author.replace('"', " "));
            let docs = self.search_docs(&[("q", q.as_str())]).await?;
            return Ok(rank_and_limit(docs, "", author));
        }
        let mut params = vec![("title", title)];
        if !author.is_empty() {
            params.push(("author", author));
        }
        let primary = self.search_docs(&params).await?;
        let ranked = rank_and_limit(primary.clone(), title, author);
        if strong_match(&ranked, title, author) {
            return Ok(ranked);
        }
        let q = if author.is_empty() {
            title.to_owned()
        } else {
            format!("{title} {author}")
        };
        match self.search_docs(&[("q", q.as_str())]).await {
            Ok(fallback) => {
                let mut all = primary;
                all.extend(fallback);
                Ok(rank_and_limit(all, title, author))
            }
            // A fallback failure must not hide the usable field-search results.
            Err(_) => Ok(ranked),
        }
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
        let cover = if let Some(id) = book.cover_id {
            Some(format!("{}/b/id/{id}-L.jpg?default=false", self.covers))
        } else {
            book.cover_olid
                .as_deref()
                .filter(|olid| valid_cover_olid(olid))
                .map(|olid| format!("{}/b/olid/{olid}-L.jpg?default=false", self.covers))
        };
        if let Some(cover) = cover {
            let url = reqwest::Url::parse(&cover).unwrap();
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
        return url.path().starts_with("/b/id/") || url.path().starts_with("/b/olid/");
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
        cover_olid: doc["cover_edition_key"]
            .as_str()
            .filter(|value| valid_cover_olid(value))
            .map(str::to_owned),
        subtitle: text(doc["subtitle"].as_str(), 300),
        publishers: strings(&doc["publisher"], 2, 160),
        languages: strings(&doc["language"], 2, 16)
            .into_iter()
            .map(|code| language_label(&code))
            .collect(),
        isbn: strings(&doc["isbn"], 3, 20),
        cover_cached: false,
        retrieved_at: Utc::now(),
        user_edited: false,
    })
}

/// Validate and compact an ISBN-10/13 (hyphens and spaces ignored). Checksums catch typos
/// before a request leaves the host.
pub fn normalize_isbn(value: &str) -> Result<String, String> {
    let compact: String = value
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .collect();
    let valid = match compact.len() {
        10 => {
            let bytes = compact.as_bytes();
            bytes[..9].iter().all(u8::is_ascii_digit)
                && (bytes[9].is_ascii_digit() || bytes[9].eq_ignore_ascii_case(&b'X'))
                && isbn10_checksum(&compact)
        }
        13 => compact.bytes().all(|byte| byte.is_ascii_digit()) && isbn13_checksum(&compact),
        _ => false,
    };
    if valid {
        Ok(compact.to_ascii_uppercase())
    } else {
        Err("Enter a valid ISBN-10 or ISBN-13.".into())
    }
}

fn isbn10_checksum(value: &str) -> bool {
    value
        .bytes()
        .enumerate()
        .try_fold(0u32, |sum, (index, byte)| {
            let digit = match byte {
                b'0'..=b'9' => u32::from(byte - b'0'),
                b'X' | b'x' if index == 9 => 10,
                _ => return None,
            };
            Some(sum + digit * (10 - index as u32))
        })
        .is_some_and(|sum| sum % 11 == 0)
}

fn isbn13_checksum(value: &str) -> bool {
    value
        .bytes()
        .enumerate()
        .try_fold(0u32, |sum, (index, byte)| {
            let digit = u32::from(byte.checked_sub(b'0')?);
            (digit <= 9).then_some(sum + digit * if index % 2 == 0 { 1 } else { 3 })
        })
        .is_some_and(|sum| sum % 10 == 0)
}

fn valid_cover_olid(value: &str) -> bool {
    value
        .strip_prefix("OL")
        .and_then(|value| value.strip_suffix('M'))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.len() <= 16 && digits.bytes().all(|b| b.is_ascii_digit())
        })
}

pub(crate) fn language_label(code: &str) -> String {
    match code {
        "en" | "eng" => "English",
        "fr" | "fre" | "fra" => "French",
        "de" | "ger" | "deu" => "German",
        "es" | "spa" => "Spanish",
        "it" | "ita" => "Italian",
        "pt" | "por" => "Portuguese",
        "nl" | "dut" | "nld" => "Dutch",
        "ru" | "rus" => "Russian",
        "ja" | "jpn" => "Japanese",
        "zh" | "chi" | "zho" => "Chinese",
        "ko" | "kor" => "Korean",
        "ar" | "ara" => "Arabic",
        "la" | "lat" => "Latin",
        "el" | "gre" | "ell" => "Greek",
        "sv" | "swe" => "Swedish",
        "pl" | "pol" => "Polish",
        "tr" | "tur" => "Turkish",
        "hi" | "hin" => "Hindi",
        "bn" | "ben" => "Bengali",
        "he" | "heb" => "Hebrew",
        _ => code,
    }
    .to_string()
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_key(value: &str) -> String {
    let normalized = normalize(value);
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = normalized.strip_prefix(article) {
            return rest.to_string();
        }
    }
    normalized
}

fn token_overlap(query: &str, candidate: &str) -> usize {
    let candidate: std::collections::HashSet<&str> = candidate.split_whitespace().collect();
    query
        .split_whitespace()
        .filter(|token| token.len() > 1 && candidate.contains(token))
        .count()
}

fn derivative(book: &Metadata) -> bool {
    let text = format!(
        " {} {} ",
        normalize(&book.title),
        book.subtitle.as_deref().map(normalize).unwrap_or_default()
    );
    DERIVATIVE_WORDS
        .iter()
        .any(|word| text.contains(&format!(" {word} ")))
}

fn author_match(book: &Metadata, author: &str) -> bool {
    let query = normalize(author);
    if query.is_empty() {
        return true;
    }
    let last = query.split_whitespace().last().unwrap_or_default();
    book.authors.iter().any(|candidate| {
        let candidate = normalize(candidate);
        !candidate.is_empty()
            && (candidate.contains(&query)
                || query.contains(&candidate)
                || (!last.is_empty() && candidate.split_whitespace().last() == Some(last)))
    })
}

fn score(book: &Metadata, title: &str, author: &str) -> i32 {
    let mut score = 0;
    if !title.trim().is_empty() {
        let query = title_key(title);
        let candidate = title_key(&book.title);
        if !query.is_empty() && candidate == query {
            score += 100;
        } else if !query.is_empty() && candidate.contains(&query) {
            score += 60;
        } else {
            // A short generic title ("Mistborn") must not outrank the fuller title the
            // owner typed ("Mistborn: The Final Empire"); shared-token count breaks that tie.
            score += (token_overlap(&query, &candidate) as i32) * 12;
        }
        if derivative(book) {
            score -= 45;
        }
    }
    if !author.trim().is_empty() && author_match(book, author) {
        score += 30;
    }
    if book.cover_id.is_some() || book.cover_olid.is_some() {
        score += 2;
    }
    if book.edition_count.unwrap_or(0) > 0 {
        score += 1;
    }
    score
}

fn rank_and_limit(docs: Vec<Metadata>, title: &str, author: &str) -> Vec<Metadata> {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for book in docs {
        if seen.insert(book.work_key.clone()) {
            unique.push(book);
        }
    }
    let mut scored: Vec<(i32, usize, Metadata)> = unique
        .into_iter()
        .enumerate()
        .map(|(index, book)| (score(&book, title, author), index, book))
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored
        .into_iter()
        .take(SEARCH_RETURN)
        .map(|(_, _, book)| book)
        .collect()
}

fn strong_match(docs: &[Metadata], title: &str, author: &str) -> bool {
    if title.trim().is_empty() {
        return docs.iter().any(|book| author_match(book, author));
    }
    let query = title_key(title);
    docs.iter().any(|book| {
        let candidate = title_key(&book.title);
        let exact = !query.is_empty() && candidate == query;
        let close = !query.is_empty()
            && candidate.starts_with(&query)
            && !derivative(book)
            && candidate.len() <= query.len() + 24;
        (exact || close) && (author.trim().is_empty() || author_match(book, author))
    })
}

/// Re-encode a bounded raster as JPEG; never store SVG, metadata or arbitrary provider bytes.
pub(crate) fn sanitize_cover(bytes: &[u8]) -> Result<Vec<u8>, String> {
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
    fn edits_validate_and_preserve_provider_identity() {
        let mut metadata = candidate(
            &json!({"key":"/works/OL1W","title":"Provider","author_name":["A"],"cover_i":4}),
        )
        .unwrap();
        let mut edits = MetadataEdits {
            title: " Local title ".into(),
            authors: vec!["New author".into()],
            first_publish_year: Some(1900),
            subjects: vec![],
            description: None,
        };
        edits.apply(&mut metadata).unwrap();
        assert_eq!(metadata.title, "Local title");
        assert!(metadata.user_edited);
        assert_eq!(metadata.work_key, "/works/OL1W");
        assert_eq!(metadata.cover_id, Some(4));
        edits.description = Some("x".repeat(1601));
        assert!(edits.apply(&mut metadata).is_err());
        edits.description = None;
        edits.authors = vec!["A".into(); 9];
        assert!(edits.apply(&mut metadata).is_err());
    }

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
    async fn search_merges_field_and_general_results_and_uses_olid_covers() {
        use axum::{Json, Router, extract::Query, http::StatusCode, routing::get};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/search.json",
            get({
                let calls = calls.clone();
                move |Query(q): Query<std::collections::HashMap<String, String>>| {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(q["limit"], "12");
                        assert!(q["fields"].contains("cover_edition_key"));
                        assert!(q["fields"].contains("isbn"));
                        if let Some(title) = q.get("title") {
                            assert_eq!(title, "Mistborn: The Final Empire");
                            assert_eq!(q["author"], "Brandon Sanderson");
                            Json(json!({"docs":[
                                {"key":"/works/OL1W","title":"Mistborn 7 Books Collection Set","author_name":["Brandon Sanderson"],"cover_i":5},
                                {"key":"/works/OL2W","title":"Mistborn","author_name":["Brandon Sanderson"]}
                            ]}))
                        } else {
                            assert_eq!(q["q"], "Mistborn: The Final Empire Brandon Sanderson");
                            Json(json!({"docs":[
                                {"key":"/works/OL3W","title":"The Final Empire","author_name":["Brandon Sanderson"],"cover_edition_key":"OL123M","language":["eng"],"publisher":["Tor"],"isbn":["9780765311788"]}
                            ]}))
                        }
                    }
                }
            }))
            .route(
                "/works/OL3W.json",
                get(|| async { Json(json!({"description":{"value":"A description"}})) }),
            )
            .route(
                "/b/olid/OL123M-L.jpg",
                get(|| async { (StatusCode::NOT_FOUND, "missing") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut catalog = Catalog::new().unwrap();
        catalog.api = base.clone();
        catalog.covers = base;
        let results = catalog
            .search(SearchTerms {
                title: "Mistborn: The Final Empire",
                author: "Brandon Sanderson",
                isbn: "",
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "primary plus fallback");
        assert_eq!(results[0].title, "The Final Empire", "{results:#?}");
        assert_eq!(results[0].cover_olid.as_deref(), Some("OL123M"));
        assert_eq!(results[0].languages, vec!["English"]);
        assert_eq!(results[0].publishers, vec!["Tor"]);
        assert_eq!(results[0].isbn, vec!["9780765311788"]);
        let (metadata, cover) = catalog.enrich(results[0].clone()).await.unwrap();
        assert_eq!(metadata.description.as_deref(), Some("A description"));
        assert!(cover.is_none());
        assert!(
            catalog
                .search(SearchTerms {
                    title: "",
                    author: "",
                    isbn: "",
                })
                .await
                .is_err()
        );
        server.abort();
    }

    #[tokio::test]
    async fn isbn_search_is_exact_and_checksum_checked() {
        use axum::{Json, Router, extract::Query, routing::get};
        let app = Router::new().route(
            "/search.json",
            get(
                |Query(q): Query<std::collections::HashMap<String, String>>| async move {
                    assert_eq!(q["q"], "isbn:9780765311788");
                    Json(json!({"docs":[
                        {"key":"/works/OL9W","title":"Mistborn","author_name":["Brandon Sanderson"]}
                    ]}))
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut catalog = Catalog::new().unwrap();
        catalog.api = base.clone();
        catalog.covers = base;
        let results = catalog
            .search(SearchTerms {
                title: "ignored",
                author: "ignored",
                isbn: "978-0-7653-1178-8",
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Mistborn");
        assert!(
            catalog
                .search(SearchTerms {
                    title: "",
                    author: "",
                    isbn: "123",
                })
                .await
                .is_err()
        );
        server.abort();
    }

    #[test]
    fn isbn_normalization_and_cover_olid_are_bounded() {
        assert_eq!(
            normalize_isbn("978-0-7653-1178-8").unwrap(),
            "9780765311788"
        );
        assert_eq!(normalize_isbn("0-7653-1178-X").unwrap(), "076531178X");
        for value in ["", "123", "9780765311789", "0-7653-1178-1", "978076531178X"] {
            assert!(normalize_isbn(value).is_err(), "{value}");
        }
        assert!(valid_cover_olid("OL123M"));
        for value in ["OL123W", "OLM", "OL123", "OL123M/extra"] {
            assert!(!valid_cover_olid(value), "{value}");
        }
    }

    #[test]
    fn ranking_prefers_the_owned_work_over_collections() {
        let query = "Mistborn: The Final Empire";
        let author = "Brandon Sanderson";
        let collection = candidate(&json!({
            "key":"/works/OL1W",
            "title":"Mistborn 7 Books Collection Set",
            "author_name":["Brandon Sanderson"],
            "cover_i":5
        }))
        .unwrap();
        let generic = candidate(&json!({
            "key":"/works/OL2W",
            "title":"Mistborn",
            "author_name":["Brandon Sanderson"]
        }))
        .unwrap();
        let intended = candidate(&json!({
            "key":"/works/OL3W",
            "title":"The Final Empire",
            "author_name":["Brandon Sanderson"]
        }))
        .unwrap();
        let ranked = rank_and_limit(vec![collection, generic, intended.clone()], query, author);
        assert_eq!(ranked[0].work_key, intended.work_key);
        assert!(strong_match(
            std::slice::from_ref(&intended),
            "The Final Empire",
            author
        ));
        assert!(!strong_match(
            &[candidate(&json!({"key":"/works/OL4W","title":"Mistborn 7 Books Collection Set","author_name":["Brandon Sanderson"]})).unwrap()],
            query,
            author
        ));
    }
}
