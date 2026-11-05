//! Spotify adapter: the owner's own Spotify account through the Spotify Web API.
//!
//! SAIL signs in with OAuth authorization code + PKCE (no client secret is stored), keeps the
//! token under the SAIL data root with owner-only permissions, and exposes search, the owner's
//! playlists, playback control and the current player state. Playback needs Spotify Premium
//! and an available Spotify Connect device; failures are reported, never simulated.
//!
//! Choosing what to play is a two-step conversation (find, then confirm): this module only
//! plays a URI that Dialogue has marked confirmed through [`Spotify::confirm`].
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ACCOUNTS: &str = "https://accounts.spotify.com";
const API: &str = "https://api.spotify.com/v1";
/// Read playback and playlists, control playback. Nothing that edits the library.
pub const SCOPES: &str = "user-read-playback-state user-modify-playback-state user-read-currently-playing playlist-read-private playlist-read-collaborative";
/// Login attempts expire; a callback after this is refused.
const LOGIN_TTL: Duration = Duration::from_secs(600);
/// A confirmation lets exactly one play of that URI happen shortly after the owner said yes.
const CONFIRM_TTL: Duration = Duration::from_secs(120);
const MAX_OWN_PLAYLISTS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Playlist,
    Track,
    Album,
    Artist,
}

impl Kind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "playlist" | "playlists" => Some(Self::Playlist),
            "track" | "tracks" | "song" | "songs" => Some(Self::Track),
            "album" | "albums" => Some(Self::Album),
            "artist" | "artists" | "band" => Some(Self::Artist),
            _ => None,
        }
    }
    fn api(self) -> &'static str {
        match self {
            Self::Playlist => "playlist",
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
        }
    }
    fn noun(self) -> &'static str {
        match self {
            Self::Track => "song",
            other => other.api(),
        }
    }
    pub fn of_uri(uri: &str) -> Option<Self> {
        let mut parts = uri.split(':');
        if parts.next()? != "spotify" {
            return None;
        }
        let kind = Self::parse(parts.next()?)?;
        let id = parts.next()?;
        (parts.next().is_none() && !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()))
            .then_some(kind)
    }
}

/// Something SAIL can offer to play, named the way the owner sees it in Spotify.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: Kind,
    pub name: String,
    /// Playlist owner, or the artists of a track or album.
    pub by: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Candidate {
    /// The exact question SAIL asks before playing, with the full name of the element.
    pub fn confirmation_question(&self) -> String {
        match self.kind {
            Kind::Playlist => format!(
                "Should we proceed by playing the playlist with the name: '{}' created by '{}'?",
                self.name, self.by
            ),
            Kind::Artist => format!(
                "Should we proceed by playing the artist with the name: '{}'?",
                self.name
            ),
            kind => format!(
                "Should we proceed by playing the {} with the name: '{}' by '{}'?",
                kind.noun(),
                self.name,
                self.by
            ),
        }
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().trim().to_string()
}
fn artists(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| text(&a["name"]))
        .filter(|n| !n.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}
fn clip(value: String, max: usize) -> Option<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some(if value.chars().count() > max {
        format!("{}…", value.chars().take(max).collect::<String>())
    } else {
        value
    })
}

/// Read one Spotify Web API object into a candidate. Search results can contain nulls.
pub fn candidate(kind: Kind, item: &Value) -> Option<Candidate> {
    let uri = text(&item["uri"]);
    let name = text(&item["name"]);
    if name.is_empty() || Kind::of_uri(&uri) != Some(kind) {
        return None;
    }
    let (by, detail) = match kind {
        Kind::Playlist => {
            let owner = &item["owner"];
            let by = Some(text(&owner["display_name"]))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| text(&owner["id"]));
            let total = item["tracks"]["total"]
                .as_u64()
                .or_else(|| item["items"]["total"].as_u64());
            let description = clip(text(&item["description"]), 120);
            let detail = match (description, total) {
                (Some(d), Some(t)) => Some(format!("{d} · {t} tracks")),
                (Some(d), None) => Some(d),
                (None, Some(t)) => Some(format!("{t} tracks")),
                (None, None) => None,
            };
            (by, detail)
        }
        Kind::Track => (
            artists(&item["artists"]),
            clip(text(&item["album"]["name"]), 120).map(|a| format!("from {a}")),
        ),
        Kind::Album => (
            artists(&item["artists"]),
            clip(text(&item["release_date"]), 10),
        ),
        Kind::Artist => (String::new(), None),
    };
    Some(Candidate {
        kind,
        name,
        by,
        uri,
        detail,
    })
}

fn words(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// How well a name answers the query: every query word found scores, an exact name wins.
pub fn score(query: &str, name: &str) -> usize {
    let q = words(query);
    let n = words(name);
    if q.is_empty() || n.is_empty() {
        return 0;
    }
    if q == n {
        return 1000;
    }
    let joined = n.join(" ");
    q.iter()
        .map(|w| {
            if n.contains(w) {
                10
            } else if w.len() > 3 && joined.contains(w.as_str()) {
                4
            } else {
                0
            }
        })
        .sum()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Token {
    access_token: String,
    refresh_token: String,
    /// Unix seconds.
    expires_at: u64,
    #[serde(default)]
    scope: String,
}

/// A Spotify Web API failure, worded for the owner.
#[derive(Debug, Clone)]
pub struct SpotifyError {
    pub message: String,
    pub code: &'static str,
}
impl SpotifyError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for SpotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
type Result<T> = std::result::Result<T, SpotifyError>;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    ring::rand::SystemRandom::new()
        .fill(&mut buf)
        .expect("system randomness");
    URL_SAFE_NO_PAD.encode(buf)
}

/// PKCE S256 challenge for a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

fn write_private(path: &std::path::Path, value: &impl Serialize) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)
}

/// The owner's Spotify connection.
pub struct Spotify {
    dir: PathBuf,
    http: reqwest::Client,
    config: Mutex<Config>,
    token: tokio::sync::Mutex<Option<Token>>,
    logins: Mutex<HashMap<String, (String, String, Instant)>>,
    confirmed: Mutex<HashMap<String, Instant>>,
    api: String,
    accounts: String,
}

impl Spotify {
    /// A connection stored in `dir` (config.json, token.json). Reads nothing from the network.
    pub fn open(dir: PathBuf) -> Self {
        Self::with_endpoints(dir, API, ACCOUNTS)
    }
    /// For tests: other API and accounts base URLs.
    pub fn with_endpoints(dir: PathBuf, api: &str, accounts: &str) -> Self {
        let mut config: Config = std::fs::read(dir.join("config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if let Ok(id) = std::env::var("SAIL_SPOTIFY_CLIENT_ID")
            && !id.trim().is_empty()
        {
            config.client_id = id.trim().into();
        }
        if let Ok(uri) = std::env::var("SAIL_SPOTIFY_REDIRECT_URI")
            && !uri.trim().is_empty()
        {
            config.redirect_uri = uri.trim().into();
        }
        let token = std::fs::read(dir.join("token.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self {
            dir,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("HTTP client"),
            config: Mutex::new(config),
            token: tokio::sync::Mutex::new(token),
            logins: Mutex::new(HashMap::new()),
            confirmed: Mutex::new(HashMap::new()),
            api: api.trim_end_matches('/').into(),
            accounts: accounts.trim_end_matches('/').into(),
        }
    }

    pub fn configured(&self) -> bool {
        let config = self.config.lock().expect("spotify config");
        !config.client_id.is_empty() && !config.redirect_uri.is_empty()
    }
    pub fn redirect_uri(&self) -> String {
        self.config
            .lock()
            .expect("spotify config")
            .redirect_uri
            .clone()
    }
    pub async fn connected(&self) -> bool {
        self.token.lock().await.is_some()
    }

    /// Save the owner's Spotify app Client ID and the redirect URI registered for it.
    pub fn configure(&self, client_id: &str, redirect_uri: &str) -> Result<()> {
        let client_id = client_id.trim();
        if client_id.len() < 16
            || client_id.len() > 64
            || !client_id.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(SpotifyError::new(
                "invalid",
                "That does not look like a Spotify Client ID (letters and digits, from your app's settings).",
            ));
        }
        let parsed = url::Url::parse(redirect_uri)
            .map_err(|_| SpotifyError::new("invalid", "The redirect URI is not a valid URL."))?;
        let loopback = matches!(parsed.host_str(), Some("127.0.0.1") | Some("[::1]"));
        if !(parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback)) {
            return Err(SpotifyError::new(
                "invalid",
                "Spotify only accepts https redirect URIs, or http on 127.0.0.1.",
            ));
        }
        let config = Config {
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
        };
        write_private(&self.dir.join("config.json"), &config).map_err(|e| {
            SpotifyError::new("storage", format!("Could not save Spotify settings: {e}"))
        })?;
        *self.config.lock().expect("spotify config") = config;
        Ok(())
    }

    /// The Spotify sign-in URL; the PKCE verifier stays here until the callback.
    pub fn login_url(&self) -> Result<String> {
        let config = self.config.lock().expect("spotify config").clone();
        if config.client_id.is_empty() || config.redirect_uri.is_empty() {
            return Err(SpotifyError::new(
                "not_configured",
                "Spotify is not set up yet.",
            ));
        }
        let verifier = random_token(48);
        let state = random_token(24);
        let challenge = pkce_challenge(&verifier);
        {
            let mut logins = self.logins.lock().expect("spotify logins");
            logins.retain(|_, (_, _, at)| at.elapsed() < LOGIN_TTL);
            if logins.len() >= 8 {
                return Err(SpotifyError::new(
                    "busy",
                    "Too many Spotify sign-ins in progress; try again in a few minutes.",
                ));
            }
            logins.insert(
                state.clone(),
                (verifier, config.redirect_uri.clone(), Instant::now()),
            );
        }
        let mut url =
            url::Url::parse(&format!("{}/authorize", self.accounts)).expect("accounts URL");
        url.query_pairs_mut()
            .append_pair("client_id", &config.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &config.redirect_uri)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge)
            .append_pair("scope", SCOPES)
            .append_pair("state", &state);
        Ok(url.into())
    }

    /// Finish sign-in with the code Spotify returned for `state`.
    pub async fn finish_login(&self, state: &str, code: &str) -> Result<()> {
        let (verifier, redirect_uri, at) = self
            .logins
            .lock()
            .expect("spotify logins")
            .remove(state)
            .ok_or_else(|| {
                SpotifyError::new(
                    "invalid",
                    "This Spotify sign-in link is unknown or was already used.",
                )
            })?;
        if at.elapsed() >= LOGIN_TTL {
            return Err(SpotifyError::new(
                "expired",
                "This Spotify sign-in expired. Start it again from SAIL.",
            ));
        }
        let client_id = self
            .config
            .lock()
            .expect("spotify config")
            .client_id
            .clone();
        let token = self
            .token_request(
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", &redirect_uri),
                    ("client_id", &client_id),
                    ("code_verifier", &verifier),
                ],
                None,
            )
            .await?;
        self.store(token).await
    }

    pub async fn disconnect(&self) -> Result<()> {
        *self.token.lock().await = None;
        match std::fs::remove_file(self.dir.join("token.json")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SpotifyError::new(
                "storage",
                format!("Could not remove the Spotify sign-in: {e}"),
            )),
        }
    }

    async fn store(&self, token: Token) -> Result<()> {
        write_private(&self.dir.join("token.json"), &token).map_err(|e| {
            SpotifyError::new(
                "storage",
                format!("Could not save the Spotify sign-in: {e}"),
            )
        })?;
        *self.token.lock().await = Some(token);
        Ok(())
    }

    async fn token_request(
        &self,
        form: &[(&str, &str)],
        previous_refresh: Option<&str>,
    ) -> Result<Token> {
        let response = self
            .http
            .post(format!("{}/api/token", self.accounts))
            .form(form)
            .send()
            .await
            .map_err(|e| {
                SpotifyError::new(
                    "unavailable",
                    format!("Spotify sign-in is unreachable: {e}"),
                )
            })?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let reason = body["error_description"]
                .as_str()
                .or(body["error"].as_str())
                .unwrap_or("refused");
            return Err(SpotifyError::new(
                "auth",
                format!("Spotify refused the sign-in ({reason}). Connect Spotify again from SAIL."),
            ));
        }
        let access_token = text(&body["access_token"]);
        let refresh_token = Some(text(&body["refresh_token"]))
            .filter(|t| !t.is_empty())
            .or_else(|| previous_refresh.map(str::to_string))
            .unwrap_or_default();
        if access_token.is_empty() || refresh_token.is_empty() {
            return Err(SpotifyError::new(
                "auth",
                "Spotify returned an incomplete sign-in.",
            ));
        }
        Ok(Token {
            access_token,
            refresh_token,
            expires_at: now()
                + body["expires_in"]
                    .as_u64()
                    .unwrap_or(3600)
                    .saturating_sub(60),
            scope: text(&body["scope"]),
        })
    }

    async fn access_token(&self, force_refresh: bool) -> Result<String> {
        let mut guard = self.token.lock().await;
        let token = guard.clone().ok_or_else(|| {
            SpotifyError::new(
                "not_connected",
                "Spotify is not connected. Connect it from the Now Playing card in SAIL.",
            )
        })?;
        if !force_refresh && token.expires_at > now() {
            return Ok(token.access_token);
        }
        let client_id = self
            .config
            .lock()
            .expect("spotify config")
            .client_id
            .clone();
        let fresh = self
            .token_request(
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &token.refresh_token),
                    ("client_id", &client_id),
                ],
                Some(&token.refresh_token),
            )
            .await?;
        write_private(&self.dir.join("token.json"), &fresh).map_err(|e| {
            SpotifyError::new(
                "storage",
                format!("Could not save the Spotify sign-in: {e}"),
            )
        })?;
        let access = fresh.access_token.clone();
        *guard = Some(fresh);
        Ok(access)
    }

    /// One Web API call; refreshes once on 401. `None` for empty (204) replies.
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>> {
        for attempt in 0..2 {
            let access = self.access_token(attempt == 1).await?;
            let mut request = self
                .http
                .request(method.clone(), format!("{}{path}", self.api))
                .bearer_auth(access);
            if let Some(body) = &body {
                request = request.json(body);
            } else if method != reqwest::Method::GET {
                request = request.header("content-length", "0");
            }
            let response = request.send().await.map_err(|e| {
                SpotifyError::new("unavailable", format!("Spotify is unreachable: {e}"))
            })?;
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                continue;
            }
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let bytes = response.bytes().await.unwrap_or_default();
            let json: Option<Value> = serde_json::from_slice(&bytes).ok();
            if status.is_success() {
                return Ok(json);
            }
            let reason = json
                .as_ref()
                .and_then(|j| {
                    j["error"]["reason"]
                        .as_str()
                        .or(j["error"]["message"].as_str())
                })
                .unwrap_or("")
                .to_string();
            return Err(match status.as_u16() {
                401 => SpotifyError::new(
                    "auth",
                    "Spotify sign-in is no longer valid. Connect Spotify again from SAIL.",
                ),
                403 if reason.contains("PREMIUM") => SpotifyError::new(
                    "premium_required",
                    "Spotify only allows playback control with a Premium account.",
                ),
                403 => SpotifyError::new(
                    "forbidden",
                    format!(
                        "Spotify refused the request{}.",
                        if reason.is_empty() {
                            String::new()
                        } else {
                            format!(" ({reason})")
                        }
                    ),
                ),
                404 if reason.contains("NO_ACTIVE_DEVICE") || path.starts_with("/me/player") => {
                    SpotifyError::new(
                        "no_device",
                        "No Spotify device is available. Open Spotify on a phone, computer or speaker, then ask again.",
                    )
                }
                429 => SpotifyError::new(
                    "rate_limited",
                    format!(
                        "Spotify is rate-limiting SAIL; try again in {} seconds.",
                        retry_after.unwrap_or_else(|| "a few".into())
                    ),
                ),
                code => SpotifyError::new(
                    "failed",
                    format!(
                        "Spotify returned HTTP {code}{}.",
                        if reason.is_empty() {
                            String::new()
                        } else {
                            format!(" ({reason})")
                        }
                    ),
                ),
            });
        }
        unreachable!("the second attempt always returns")
    }

    async fn get(&self, path: &str) -> Result<Value> {
        Ok(self
            .call(reqwest::Method::GET, path, None)
            .await?
            .unwrap_or(Value::Null))
    }

    /// The owner's playlists (created or followed), as candidates.
    pub async fn my_playlists(&self) -> Result<Vec<Candidate>> {
        let mut out = Vec::new();
        let mut offset = 0;
        while out.len() < MAX_OWN_PLAYLISTS {
            let page = self
                .get(&format!("/me/playlists?limit=50&offset={offset}"))
                .await?;
            let items = page["items"].as_array().cloned().unwrap_or_default();
            out.extend(items.iter().filter_map(|i| candidate(Kind::Playlist, i)));
            if items.len() < 50 || page["next"].is_null() {
                break;
            }
            offset += 50;
        }
        out.truncate(MAX_OWN_PLAYLISTS);
        Ok(out)
    }

    /// Catalog search for one kind, best first as Spotify ranks them.
    pub async fn search(&self, query: &str, kind: Kind, limit: usize) -> Result<Vec<Candidate>> {
        let mut url = url::Url::parse("http://x/search").expect("search URL");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("type", kind.api())
            .append_pair("limit", &limit.clamp(1, 10).to_string());
        let path = format!("/search?{}", url.query().unwrap_or_default());
        let body = self.get(&path).await?;
        Ok(body[format!("{}s", kind.api())]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| candidate(kind, i))
            .collect())
    }

    /// Mark a URI the owner just confirmed; the next [`Self::play`] of it may proceed.
    pub fn confirm(&self, uri: &str) {
        let mut confirmed = self.confirmed.lock().expect("spotify confirmations");
        confirmed.retain(|_, at| at.elapsed() < CONFIRM_TTL);
        confirmed.insert(uri.into(), Instant::now());
    }

    /// Play a confirmed URI on the active device, or the first available one.
    pub async fn play(&self, uri: &str) -> Result<Value> {
        let kind = Kind::of_uri(uri).ok_or_else(|| {
            SpotifyError::new(
                "invalid",
                "That is not a Spotify playlist, song, album or artist.",
            )
        })?;
        let fresh = self
            .confirmed
            .lock()
            .expect("spotify confirmations")
            .remove(uri)
            .is_some_and(|at| at.elapsed() < CONFIRM_TTL);
        if !fresh {
            return Err(SpotifyError::new(
                "not_confirmed",
                "Not played: the owner has not confirmed this choice. Find it with spotify_find and ask the confirmation question first.",
            ));
        }
        let devices = self.get("/me/player/devices").await?;
        let list = devices["devices"].as_array().cloned().unwrap_or_default();
        let usable: Vec<&Value> = list.iter().filter(|d| d["is_restricted"] != true).collect();
        let active = usable.iter().find(|d| d["is_active"] == true);
        let target = active.or_else(|| {
            usable
                .iter()
                .find(|d| text(&d["name"]).to_lowercase().contains("sail"))
                .or(usable.first())
        });
        let Some(device) = target else {
            return Err(SpotifyError::new(
                "no_device",
                "No Spotify device is available. Open Spotify on a phone, computer or speaker, then ask again.",
            ));
        };
        let device_id = text(&device["id"]);
        let body = if kind == Kind::Track {
            json!({"uris": [uri]})
        } else {
            json!({"context_uri": uri})
        };
        let path = if active.is_some() {
            "/me/player/play".to_string()
        } else {
            format!("/me/player/play?device_id={device_id}")
        };
        self.call(reqwest::Method::PUT, &path, Some(body)).await?;
        Ok(json!({"playing": uri, "device": text(&device["name"])}))
    }

    /// pause, resume, next or previous.
    pub async fn control(&self, action: &str) -> Result<()> {
        let (method, path) = match action {
            "pause" => (reqwest::Method::PUT, "/me/player/pause"),
            "resume" | "play" => (reqwest::Method::PUT, "/me/player/play"),
            "next" => (reqwest::Method::POST, "/me/player/next"),
            "previous" => (reqwest::Method::POST, "/me/player/previous"),
            _ => {
                return Err(SpotifyError::new(
                    "invalid",
                    "Use pause, resume, next or previous.",
                ));
            }
        };
        self.call(method, path, None).await.map(|_| ())
    }

    /// What is playing now, or `null` when nothing is.
    pub async fn now_playing(&self) -> Result<Value> {
        let state = self.get("/me/player").await?;
        Ok(player_summary(&state))
    }
}

/// The parts of `/me/player` the UI and the model need.
pub fn player_summary(state: &Value) -> Value {
    if state.is_null() || state["item"].is_null() {
        return Value::Null;
    }
    let item = &state["item"];
    let image = item["album"]["images"]
        .as_array()
        .and_then(|images| {
            images
                .iter()
                .min_by_key(|i| i["width"].as_u64().unwrap_or(u64::MAX).abs_diff(128))
        })
        .map(|i| text(&i["url"]))
        .filter(|u| u.starts_with("https://"));
    json!({
        "is_playing": state["is_playing"] == true,
        "name": text(&item["name"]),
        "by": Some(artists(&item["artists"])).filter(|a| !a.is_empty()).unwrap_or_else(|| text(&item["show"]["name"])),
        "album": text(&item["album"]["name"]),
        "image": image,
        "uri": text(&item["uri"]),
        "progress_ms": state["progress_ms"],
        "duration_ms": item["duration_ms"],
        "device": text(&state["device"]["name"]),
        "context": text(&state["context"]["uri"]),
    })
}

/// The process-wide connection, stored under `<data root>/spotify`.
pub fn global() -> Arc<Spotify> {
    static SPOTIFY: OnceLock<Arc<Spotify>> = OnceLock::new();
    SPOTIFY
        .get_or_init(|| Arc::new(Spotify::open(sail_common::storage::data_path("spotify"))))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_question_names_the_playlist_and_its_creator() {
        let c = candidate(
            Kind::Playlist,
            &json!({"name":"Cabeça de Radio","uri":"spotify:playlist:37i9dQZF1DX","owner":{"display_name":"Pedro Victor","id":"pedro"},"description":"","tracks":{"total":42}}),
        )
        .unwrap();
        assert_eq!(
            c.confirmation_question(),
            "Should we proceed by playing the playlist with the name: 'Cabeça de Radio' created by 'Pedro Victor'?"
        );
        assert_eq!(c.detail.as_deref(), Some("42 tracks"));
    }

    #[test]
    fn tracks_and_albums_name_their_artists() {
        let t = candidate(Kind::Track, &json!({"name":"How to Disappear Completely","uri":"spotify:track:2rtGaCAeYal","artists":[{"name":"Radiohead"}],"album":{"name":"Kid A"}})).unwrap();
        assert_eq!(
            t.confirmation_question(),
            "Should we proceed by playing the song with the name: 'How to Disappear Completely' by 'Radiohead'?"
        );
        assert!(candidate(Kind::Track, &Value::Null).is_none());
        assert!(candidate(Kind::Track, &json!({"name":"x","uri":"spotify:album:abc"})).is_none());
    }

    #[test]
    fn uris_are_strictly_spotify_ids() {
        assert_eq!(
            Kind::of_uri("spotify:playlist:37i9dQZF1DX"),
            Some(Kind::Playlist)
        );
        assert_eq!(Kind::of_uri("spotify:track:abc:extra"), None);
        assert_eq!(Kind::of_uri("spotify:track:ab/c"), None);
        assert_eq!(Kind::of_uri("https://open.spotify.com/track/abc"), None);
    }

    #[test]
    fn scoring_prefers_exact_then_whole_words() {
        assert!(score("radiohead", "Radiohead") > score("radiohead", "Radiohead Essentials"));
        assert!(score("radiohead", "Radiohead Essentials") > score("radiohead", "Cabeça de Radio"));
        assert_eq!(score("radiohead", "Lo-fi beats"), 0);
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn player_summary_reads_the_current_item() {
        let s = player_summary(
            &json!({"is_playing":true,"progress_ms":1000,"device":{"name":"Kitchen"},
            "item":{"name":"How to Disappear Completely","uri":"spotify:track:x","duration_ms":356000,
            "artists":[{"name":"Radiohead"}],"album":{"name":"Kid A","images":[{"url":"https://i.scdn.co/a","width":640},{"url":"https://i.scdn.co/b","width":64}]}}}),
        );
        assert_eq!(s["by"], "Radiohead");
        assert_eq!(s["image"], "https://i.scdn.co/b");
        assert_eq!(s["device"], "Kitchen");
        assert!(player_summary(&Value::Null).is_null());
    }

    #[tokio::test]
    async fn play_refuses_unconfirmed_uris_before_any_network_call() {
        let dir = std::env::temp_dir().join(format!("sail-spotify-test-{}", random_token(6)));
        let spotify =
            Spotify::with_endpoints(dir.clone(), "http://127.0.0.1:9", "http://127.0.0.1:9");
        let err = spotify.play("spotify:playlist:abc").await.unwrap_err();
        assert_eq!(err.code, "not_confirmed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn configuration_requires_an_acceptable_redirect() {
        let dir = std::env::temp_dir().join(format!("sail-spotify-test-{}", random_token(6)));
        let spotify =
            Spotify::with_endpoints(dir.clone(), "http://127.0.0.1:9", "http://127.0.0.1:9");
        let id = "0123456789abcdef0123456789abcdef";
        assert!(
            spotify
                .configure(id, "http://localhost:8888/callback")
                .is_err()
        );
        assert!(
            spotify
                .configure("short", "https://sail.lan/api/spotify/callback")
                .is_err()
        );
        spotify
            .configure(id, "https://127.0.0.1:8088/api/spotify/callback")
            .unwrap();
        assert!(spotify.configured());
        let url = spotify.login_url().unwrap();
        assert!(url.starts_with("http://127.0.0.1:9/authorize?client_id=0123"));
        assert!(url.contains("code_challenge_method=S256"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("config.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "config must be owner-only");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
