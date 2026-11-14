//! Downloading catalogs and item bundles (round 20, Claude). Every request goes through
//! [`Http`], so tests use an in-memory fake and never touch the network.
//!
//! * **GitHub** ([`fetch_github`]): resolve the commit (`reference`, or
//!   `GET api.github.com/repos/{repo}/commits/HEAD` for skills), list it once with
//!   `GET .../git/trees/{commit}?recursive=1`, then read each file from
//!   `raw.githubusercontent.com/{repo}/{commit}/{path}`, which does not count against the
//!   API limit and is immutable at a commit. Symlinks (mode `120000`) refuse the bundle;
//!   submodules, dotfiles, `__pycache__` and `*.pyc` are skipped, as Hermes does. Limits
//!   are checked from the tree's sizes before any file is downloaded.
//! * **ClawHub** ([`fetch_clawhub`]): `GET /skills/{slug}` for the latest version, `GET
//!   /skills/{slug}/versions/{v}` for its file list with SHA-256s, then `GET /download`
//!   (a zip). Every listed file must be in the zip with the listed hash; other zip members
//!   (ClawHub's `_meta.json`) are ignored.
use super::{Bundle, MAX_BUNDLE_BYTES, MAX_BUNDLE_FILES, Route, USER_AGENT, safe_relative_path};
use async_trait::async_trait;
use serde_json::Value;
use std::{io::Read, sync::Arc, time::Duration};

const GITHUB_API: &str = "https://api.github.com";
const GITHUB_RAW: &str = "https://raw.githubusercontent.com";
const CLAWHUB_API: &str = "https://clawhub.ai/api/v1";
const API_JSON_LIMIT: u64 = 16 * 1024 * 1024;
const CLAWHUB_ZIP_LIMIT: u64 = 25 * 1024 * 1024;
const PARALLEL_FILES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// HTTP status with a short body excerpt.
    Status(u16, String),
    /// GitHub/ClawHub said to slow down; the message names when to retry if known.
    RateLimited(String),
    TooLarge(String),
    Network(String),
    Invalid(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(code, body) if body.is_empty() => write!(f, "HTTP {code}"),
            Self::Status(code, body) => write!(f, "HTTP {code}: {body}"),
            Self::RateLimited(m) => write!(f, "rate limited: {m}"),
            Self::TooLarge(m) => write!(f, "too large: {m}"),
            Self::Network(m) => write!(f, "network error: {m}"),
            Self::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl From<FetchError> for String {
    fn from(e: FetchError) -> Self {
        e.to_string()
    }
}

#[async_trait]
pub trait Http: Send + Sync {
    /// GET `url` and return the body, failing past `max_bytes`.
    async fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError>;
}

/// The real client: rustls, a generic User-Agent, no cookies, 60 s per request.
pub struct ReqwestHttp {
    client: reqwest::Client,
    github_token: Option<String>,
}

impl ReqwestHttp {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| e.to_string())?;
        // An owner-provided token only raises GitHub's API limit (60 → 5000 an hour).
        let github_token = std::env::var("SAIL_MARKET_GITHUB_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty());
        Ok(Self {
            client,
            github_token,
        })
    }
}

#[async_trait]
impl Http for ReqwestHttp {
    async fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError> {
        let mut request = self.client.get(url);
        if url.starts_with(GITHUB_API) {
            request = request.header("Accept", "application/vnd.github+json");
            if let Some(token) = &self.github_token {
                request = request.bearer_auth(token);
            }
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| FetchError::Network(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        if status == 429
            || (status == 403
                && response
                    .headers()
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v == "0"))
        {
            let header = |name: &str| {
                response
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<i64>().ok())
            };
            // GitHub sends the reset as a Unix time; others send Retry-After in seconds.
            let wait = header("x-ratelimit-reset")
                .map(|at| at - chrono::Utc::now().timestamp())
                .or_else(|| header("retry-after"));
            let when = match wait {
                Some(secs) if secs > 90 => format!("try again in {} minutes", (secs + 59) / 60),
                Some(_) => "try again in a minute".to_string(),
                None => "try again later".to_string(),
            };
            let hint = if url.starts_with(GITHUB_API) && self.github_token.is_none() {
                " GitHub allows 60 anonymous requests an hour; SAIL_MARKET_GITHUB_TOKEN raises that."
            } else {
                ""
            };
            return Err(FetchError::RateLimited(format!("{when}.{hint}")));
        }
        if response.content_length().is_some_and(|n| n > max_bytes) {
            return Err(FetchError::TooLarge(format!(
                "{url} is over {max_bytes} bytes"
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| FetchError::Network(e.without_url().to_string()))?
        {
            if body.len() as u64 + chunk.len() as u64 > max_bytes {
                return Err(FetchError::TooLarge(format!(
                    "{url} is over {max_bytes} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        if !(200..300).contains(&status) {
            let excerpt: String = String::from_utf8_lossy(&body).chars().take(160).collect();
            return Err(FetchError::Status(status, excerpt));
        }
        Ok(body)
    }
}

fn json(bytes: &[u8], what: &str) -> Result<Value, FetchError> {
    serde_json::from_slice(bytes).map_err(|e| FetchError::Invalid(format!("{what}: {e}")))
}

/// Percent-encode one URL path while keeping `/` separators.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn encode_query(value: &str) -> String {
    encode_path(value).replace('/', "%2F")
}

fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Hermes skips these when copying a skill or plugin.
fn skipped(path: &str) -> bool {
    path.split('/')
        .any(|c| c.starts_with('.') || c == "__pycache__")
        || path.ends_with(".pyc")
}

/// Download the bundle a route points to.
pub async fn fetch(http: Arc<dyn Http>, route: &Route) -> Result<Bundle, String> {
    match route {
        Route::Github {
            repo,
            path,
            reference,
        } => fetch_github(http, repo, path, reference.as_deref()).await,
        Route::Clawhub { slug, owner } => {
            fetch_clawhub(http.as_ref(), slug, owner.as_deref()).await
        }
        Route::Unsupported { reason } => Err(reason.clone()),
    }
}

pub async fn fetch_github(
    http: Arc<dyn Http>,
    repo: &str,
    path: &str,
    reference: Option<&str>,
) -> Result<Bundle, String> {
    let commit = match reference {
        Some(sha) if is_hex40(sha) => sha.to_ascii_lowercase(),
        Some(other) => return Err(format!("reference {other:?} is not a full commit SHA")),
        None => {
            let body = http
                .get(
                    &format!("{GITHUB_API}/repos/{repo}/commits/HEAD"),
                    API_JSON_LIMIT,
                )
                .await?;
            let sha = json(&body, "commit")?["sha"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !is_hex40(&sha) {
                return Err(format!("GitHub returned no commit for {repo}"));
            }
            sha
        }
    };
    let body = http
        .get(
            &format!("{GITHUB_API}/repos/{repo}/git/trees/{commit}?recursive=1"),
            API_JSON_LIMIT,
        )
        .await?;
    let tree = json(&body, "tree")?;
    if tree["truncated"].as_bool() == Some(true) {
        return Err(format!(
            "{repo} is too large for GitHub to list in one request"
        ));
    }
    let prefix = path.trim_end_matches('/');
    let mut wanted = Vec::new();
    let mut total = 0u64;
    for item in tree["tree"].as_array().into_iter().flatten() {
        let full = item["path"].as_str().unwrap_or_default();
        let rel = if prefix.is_empty() {
            full
        } else if let Some(rest) = full.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
            rest
        } else {
            continue;
        };
        if rel.is_empty() || skipped(rel) {
            continue;
        }
        match (item["type"].as_str(), item["mode"].as_str()) {
            (Some("blob"), Some("120000")) => {
                return Err(format!(
                    "{rel} is a symbolic link; SAIL does not install those"
                ));
            }
            (Some("blob"), _) => {}
            _ => continue, // trees and submodules
        }
        if !safe_relative_path(rel) {
            return Err(format!("unsafe file path {rel:?}"));
        }
        total += item["size"].as_u64().unwrap_or(0);
        wanted.push((full.to_string(), rel.to_string()));
    }
    if wanted.is_empty() {
        return Err(format!(
            "no files under {repo}/{prefix} at {}",
            &commit[..12]
        ));
    }
    if wanted.len() > MAX_BUNDLE_FILES || total > MAX_BUNDLE_BYTES {
        return Err(format!(
            "{} files, {} bytes: over the {MAX_BUNDLE_FILES}-file / 32 MiB limit",
            wanted.len(),
            total
        ));
    }
    let semaphore = Arc::new(tokio::sync::Semaphore::new(PARALLEL_FILES));
    let mut tasks = tokio::task::JoinSet::new();
    for (full, rel) in wanted {
        let http = http.clone();
        let semaphore = semaphore.clone();
        let url = format!("{GITHUB_RAW}/{repo}/{commit}/{}", encode_path(&full));
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await;
            http.get(&url, MAX_BUNDLE_BYTES).await.map(|b| (rel, b))
        });
    }
    let mut bundle = Bundle {
        files: Default::default(),
        revision: commit,
    };
    while let Some(done) = tasks.join_next().await {
        let (rel, bytes) = done.map_err(|e| e.to_string())??;
        bundle.files.insert(rel, bytes);
        if bundle.total_bytes() > MAX_BUNDLE_BYTES {
            return Err("the files are larger than 32 MiB".into());
        }
    }
    Ok(bundle)
}

pub async fn fetch_clawhub(
    http: &dyn Http,
    slug: &str,
    owner: Option<&str>,
) -> Result<Bundle, String> {
    let owner_q = owner
        .map(|o| format!("owner={}", encode_query(o)))
        .unwrap_or_default();
    let with_owner = |base: String| {
        if owner_q.is_empty() {
            base
        } else if base.contains('?') {
            format!("{base}&{owner_q}")
        } else {
            format!("{base}?{owner_q}")
        }
    };
    let slug_q = encode_query(slug);
    let detail = json(
        &http
            .get(
                &with_owner(format!("{CLAWHUB_API}/skills/{slug_q}")),
                API_JSON_LIMIT,
            )
            .await?,
        "ClawHub skill",
    )?;
    if detail["moderation"]["isMalwareBlocked"].as_bool() == Some(true) {
        return Err("ClawHub has blocked this skill as malware".into());
    }
    let version = detail["latestVersion"]["version"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() < 64)
        .ok_or("ClawHub lists no version for this skill")?
        .to_string();
    let version_q = encode_query(&version);
    let meta = json(
        &http
            .get(
                &with_owner(format!(
                    "{CLAWHUB_API}/skills/{slug_q}/versions/{version_q}"
                )),
                API_JSON_LIMIT,
            )
            .await?,
        "ClawHub version",
    )?;
    let listed: Vec<(String, String)> = meta["version"]["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some((
                f["path"].as_str()?.to_string(),
                f["sha256"].as_str()?.to_ascii_lowercase(),
            ))
        })
        .collect();
    if listed.is_empty() {
        return Err("ClawHub lists no files for this version".into());
    }
    if listed.len() > MAX_BUNDLE_FILES {
        return Err("too many files".into());
    }
    let zip_bytes = http
        .get(
            &with_owner(format!(
                "{CLAWHUB_API}/download?slug={slug_q}&version={version_q}"
            )),
            CLAWHUB_ZIP_LIMIT,
        )
        .await?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| format!("ClawHub download is not a zip: {e}"))?;
    let mut bundle = Bundle {
        files: Default::default(),
        revision: version,
    };
    for (path, sha) in listed {
        if !safe_relative_path(&path) {
            return Err(format!("unsafe file path {path:?}"));
        }
        let mut entry = archive
            .by_name(&path)
            .map_err(|_| format!("{path} is listed but missing from the download"))?;
        if entry.is_symlink() || entry.is_dir() {
            return Err(format!("{path} is not a regular file"));
        }
        let mut bytes = Vec::new();
        (&mut entry)
            .take(MAX_BUNDLE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if hex_sha256(&bytes) != sha {
            return Err(format!("{path} does not match the hash ClawHub published"));
        }
        bundle.files.insert(path, bytes);
        if bundle.total_bytes() > MAX_BUNDLE_BYTES {
            return Err("the files are larger than 32 MiB".into());
        }
    }
    Ok(bundle)
}

pub fn hex_sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// In-memory [`Http`] for tests and the offline demo: exact URL → body or status.
#[derive(Default)]
pub struct FakeHttp {
    pub responses: std::sync::Mutex<std::collections::BTreeMap<String, Result<Vec<u8>, u16>>>,
    pub requests: std::sync::Mutex<Vec<String>>,
}

impl FakeHttp {
    pub fn insert(&self, url: impl Into<String>, body: impl Into<Vec<u8>>) {
        self.responses
            .lock()
            .unwrap()
            .insert(url.into(), Ok(body.into()));
    }

    pub fn fail(&self, url: impl Into<String>, status: u16) {
        self.responses
            .lock()
            .unwrap()
            .insert(url.into(), Err(status));
    }

    /// Serve a GitHub repository at `commit` with `files` (path → content); also answers
    /// `commits/HEAD`.
    pub fn github_repo(&self, repo: &str, commit: &str, files: &[(&str, &[u8])]) {
        self.insert(
            format!("{GITHUB_API}/repos/{repo}/commits/HEAD"),
            serde_json::json!({ "sha": commit }).to_string(),
        );
        let tree: Vec<Value> = files
            .iter()
            .map(|(p, b)| serde_json::json!({"path": p, "type": "blob", "mode": "100644", "size": b.len()}))
            .collect();
        self.insert(
            format!("{GITHUB_API}/repos/{repo}/git/trees/{commit}?recursive=1"),
            serde_json::json!({"sha": commit, "tree": tree, "truncated": false}).to_string(),
        );
        for (p, b) in files {
            self.insert(
                format!("{GITHUB_RAW}/{repo}/{commit}/{}", encode_path(p)),
                *b,
            );
        }
    }
}

#[async_trait]
impl Http for FakeHttp {
    async fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError> {
        self.requests.lock().unwrap().push(url.to_string());
        match self.responses.lock().unwrap().get(url) {
            Some(Ok(body)) if body.len() as u64 > max_bytes => {
                Err(FetchError::TooLarge(url.to_string()))
            }
            Some(Ok(body)) => Ok(body.clone()),
            Some(Err(status)) => Err(FetchError::Status(*status, String::new())),
            None => Err(FetchError::Status(404, format!("no fake for {url}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: &str = "0123456789abcdef0123456789abcdef01234567";

    #[tokio::test]
    async fn github_bundle_is_the_skill_folder_only() {
        let http = Arc::new(FakeHttp::default());
        http.github_repo(
            "o/skills",
            C,
            &[
                ("skills/pdf/SKILL.md", b"---\nname: pdf\n---\n"),
                ("skills/pdf/references/a b.md", b"ref"),
                ("skills/pdf/.DS_Store", b"x"),
                ("skills/pdf/__pycache__/x.pyc", b"x"),
                ("skills/pdfx/SKILL.md", b"other"),
                ("README.md", b"root"),
            ],
        );
        let bundle = fetch_github(http.clone(), "o/skills", "skills/pdf/", None)
            .await
            .unwrap();
        assert_eq!(bundle.revision, C);
        assert_eq!(
            bundle.files.keys().collect::<Vec<_>>(),
            ["SKILL.md", "references/a b.md"]
        );
        assert!(
            http.requests
                .lock()
                .unwrap()
                .iter()
                .any(|u| u.ends_with("/skills/pdf/references/a%20b.md"))
        );
    }

    #[tokio::test]
    async fn github_refuses_symlinks_and_short_refs() {
        let http = Arc::new(FakeHttp::default());
        http.insert(
            format!("{GITHUB_API}/repos/o/p/git/trees/{C}?recursive=1"),
            serde_json::json!({"tree":[{"path":"link","type":"blob","mode":"120000","size":4}],"truncated":false}).to_string(),
        );
        let err = fetch_github(http.clone(), "o/p", "", Some(C))
            .await
            .unwrap_err();
        assert!(err.contains("symbolic link"), "{err}");
        let err = fetch_github(http, "o/p", "", Some("main"))
            .await
            .unwrap_err();
        assert!(err.contains("full commit"), "{err}");
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (p, b) in files {
            zip.start_file(*p, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[tokio::test]
    async fn clawhub_checks_every_listed_hash() {
        let http = FakeHttp::default();
        let skill = b"---\nname: dictionary\ndescription: d\n---\n".as_slice();
        http.insert(
            format!("{CLAWHUB_API}/skills/dict"),
            r#"{"skill":{"slug":"dict"},"latestVersion":{"version":"1.0.0"}}"#,
        );
        http.insert(
            format!("{CLAWHUB_API}/skills/dict/versions/1.0.0"),
            serde_json::json!({"version":{"files":[{"path":"SKILL.md","sha256":hex_sha256(skill)}]}}).to_string(),
        );
        http.insert(
            format!("{CLAWHUB_API}/download?slug=dict&version=1.0.0"),
            zip_of(&[("SKILL.md", skill), ("_meta.json", b"{}")]),
        );
        let bundle = fetch_clawhub(&http, "dict", None).await.unwrap();
        assert_eq!(bundle.files.keys().collect::<Vec<_>>(), ["SKILL.md"]);
        assert_eq!(bundle.revision, "1.0.0");

        http.insert(
            format!("{CLAWHUB_API}/download?slug=dict&version=1.0.0"),
            zip_of(&[("SKILL.md", b"tampered")]),
        );
        let err = fetch_clawhub(&http, "dict", None).await.unwrap_err();
        assert!(err.contains("does not match"), "{err}");
    }
}
