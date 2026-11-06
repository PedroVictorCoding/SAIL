//! Spotify adapter: the owner's own Spotify account through the Spotify Web API.
//!
//! SAIL signs in with OAuth authorization code + PKCE (no client secret is stored), keeps the
//! token under the SAIL data root with owner-only permissions, and exposes search, the owner's
//! playlists, playback control and the current player state. Playback needs Spotify Premium
//! and an available Spotify Connect device; failures are reported, never simulated.
//!
//! Asking for music plays it: Dialogue finds the best match and plays it in the same turn.
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
pub const SCOPES: &str = "user-read-playback-state user-modify-playback-state user-read-currently-playing playlist-read-private playlist-read-collaborative streaming user-read-email user-read-private";
/// The scope the in-browser player (Spotify Web Playback SDK) needs. Connections made before
/// the SAIL music player lack it and must be reconnected once.
pub const PLAYER_SCOPE: &str = "streaming";
/// Login attempts expire; a callback after this is refused.
const LOGIN_TTL: Duration = Duration::from_secs(600);
const MAX_OWN_PLAYLISTS: usize = 200;
/// The playlist index is rebuilt in the background when older than this.
const INDEX_TTL_SECS: u64 = 6 * 3600;
/// Tracks read per playlist for its artist profile.
const INDEX_TRACKS_PER_PLAYLIST: usize = 300;
/// An artist this share of a playlist's tracks makes it "their" playlist.
const ARTIST_SHARE_MATCH: f32 = 0.25;

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
    /// What SAIL says it is playing, with the full name: "the playlist 'Cabeça de Radio' by
    /// Pedro Victor".
    pub fn describe(&self) -> String {
        match self.kind {
            Kind::Artist => format!("the artist '{}'", self.name),
            _ if self.by.is_empty() => format!("the {} '{}'", self.kind.noun(), self.name),
            kind => format!("the {} '{}' by {}", kind.noun(), self.name, self.by),
        }
    }
}

/// A Spotify track as a SAIL music-player item. Local files, episodes and unplayable
/// tracks are skipped.
pub fn player_track(t: &Value) -> Option<Value> {
    let uri = text(&t["uri"]);
    if Kind::of_uri(&uri) != Some(Kind::Track) || t["is_local"] == true || t["is_playable"] == false
    {
        return None;
    }
    let artists: Vec<String> = t["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| text(&a["name"]))
        .filter(|n| !n.is_empty())
        .collect();
    let art = t["album"]["images"]
        .as_array()
        .and_then(|i| i.last().or(i.first()))
        .map(|i| text(&i["url"]))
        .unwrap_or_default();
    let art_large = t["album"]["images"][0]["url"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    Some(json!({
        "source": "spotify",
        "id": uri,
        "title": text(&t["name"]),
        "artist": artists.join(", "),
        "album": text(&t["album"]["name"]),
        "duration_ms": t["duration_ms"].as_u64().unwrap_or(0),
        "art": if art_large.is_empty() { art } else { art_large },
    }))
}

/// Spotify ids (tracks, devices) are short base62 or hex strings.
fn valid_device_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
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
    index: Mutex<Option<PlaylistIndex>>,
    indexing: std::sync::atomic::AtomicBool,
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
        let index = std::fs::read(dir.join("playlist_index.json"))
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
            index: Mutex::new(index),
            indexing: std::sync::atomic::AtomicBool::new(false),
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
        *self.index.lock().expect("spotify index") = None;
        let _ = std::fs::remove_file(self.dir.join("playlist_index.json"));
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

    /// Play a URI on the active device, or the first available one.
    pub async fn play(&self, uri: &str) -> Result<Value> {
        let kind = Kind::of_uri(uri).ok_or_else(|| {
            SpotifyError::new(
                "invalid",
                "That is not a Spotify playlist, song, album or artist.",
            )
        })?;
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

    /// Whether the stored sign-in carries the in-browser player's scope.
    pub async fn can_stream(&self) -> bool {
        self.token
            .lock()
            .await
            .as_ref()
            .is_some_and(|t| t.scope.split_whitespace().any(|s| s == PLAYER_SCOPE))
    }

    /// A current access token and its expiry (Unix seconds) for the owner's in-browser
    /// player. Refreshes when needed. Refused when the sign-in lacks the streaming scope.
    pub async fn player_token(&self) -> Result<(String, u64)> {
        if !self.can_stream().await {
            return Err(SpotifyError::new(
                "reconnect",
                "Reconnect Spotify once so SAIL's player can play in this browser.",
            ));
        }
        let access = self.access_token(false).await?;
        let expires_at = self
            .token
            .lock()
            .await
            .as_ref()
            .map(|t| t.expires_at)
            .unwrap_or(0);
        Ok((access, expires_at))
    }

    /// Play these track URIs on one device (the browser player), starting at `position_ms`.
    /// Spotify plays only these tracks, so SAIL's queue decides what comes next.
    pub async fn play_on(&self, device_id: &str, uris: &[String], position_ms: u64) -> Result<()> {
        if !valid_device_id(device_id) {
            return Err(SpotifyError::new(
                "invalid",
                "That is not a Spotify device id.",
            ));
        }
        if uris.is_empty()
            || uris.len() > 50
            || uris.iter().any(|u| Kind::of_uri(u) != Some(Kind::Track))
        {
            return Err(SpotifyError::new(
                "invalid",
                "Send between 1 and 50 Spotify track URIs.",
            ));
        }
        let path = format!("/me/player/play?device_id={device_id}");
        self.call(
            reqwest::Method::PUT,
            &path,
            Some(json!({"uris": uris, "position_ms": position_ms})),
        )
        .await
        .map(|_| ())
    }

    /// Track search for the music player: up to `limit` tracks, as player items.
    pub async fn search_tracks(&self, query: &str, limit: usize) -> Result<Vec<Value>> {
        let mut url = url::Url::parse("http://x/search").expect("search URL");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("type", "track")
            .append_pair("limit", &limit.clamp(1, 20).to_string());
        let body = self
            .get(&format!("/search?{}", url.query().unwrap_or_default()))
            .await?;
        Ok(body["tracks"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(player_track)
            .collect())
    }

    /// The tracks of an album or playlist (first 100), as player items.
    pub async fn tracks_of(&self, uri: &str) -> Result<Vec<Value>> {
        let (kind, id) = match (Kind::of_uri(uri), uri.rsplit(':').next()) {
            (Some(k @ (Kind::Album | Kind::Playlist)), Some(id)) if valid_device_id(id) => (k, id),
            _ => {
                return Err(SpotifyError::new(
                    "invalid",
                    "Send a Spotify album or playlist URI.",
                ));
            }
        };
        if kind == Kind::Album {
            let album = self.get(&format!("/albums/{id}")).await?;
            let art = album["images"][0]["url"].clone();
            let name = album["name"].clone();
            return Ok(album["tracks"]["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| {
                    let mut t = t.clone();
                    t["album"] = json!({"name": name, "images": [{"url": art}]});
                    player_track(&t)
                })
                .collect());
        }
        let page = self
            .get(&format!("/playlists/{id}/tracks?limit=100"))
            .await?;
        Ok(page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| player_track(&i["track"]))
            .collect())
    }

    /// Search every kind at once and pick what a plain "play X" most likely means: an exact
    /// artist, song, album or playlist name, in that order, else Spotify's top song.
    pub async fn search_any(&self, query: &str) -> Result<Vec<Candidate>> {
        let mut url = url::Url::parse("http://x/search").expect("search URL");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("type", "artist,track,album,playlist")
            .append_pair("limit", "3");
        let body = self
            .get(&format!("/search?{}", url.query().unwrap_or_default()))
            .await?;
        let mut all: Vec<Candidate> = Vec::new();
        for kind in [Kind::Artist, Kind::Track, Kind::Album, Kind::Playlist] {
            all.extend(
                body[format!("{}s", kind.api())]["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| candidate(kind, i)),
            );
        }
        let exact = |c: &Candidate| score(query, &c.name) >= 1000;
        if let Some(pos) = all.iter().position(exact) {
            let best = all.remove(pos);
            all.insert(0, best);
        } else if let Some(pos) = all.iter().position(|c| c.kind == Kind::Track) {
            let best = all.remove(pos);
            all.insert(0, best);
        }
        all.truncate(5);
        Ok(all)
    }

    /// The artists in one playlist, counted over its first tracks.
    async fn playlist_artists(&self, uri: &str) -> Result<(u32, Vec<(String, u32)>)> {
        let id = uri.rsplit(':').next().unwrap_or_default();
        let mut counts: HashMap<String, u32> = HashMap::new();
        let mut tracks = 0u32;
        let mut offset = 0;
        while (tracks as usize) < INDEX_TRACKS_PER_PLAYLIST {
            let path = format!(
                "/playlists/{id}/tracks?limit=100&offset={offset}&fields=items(track(artists(name)),item(artists(name))),next"
            );
            let page = self.get(&path).await?;
            let items = page["items"].as_array().cloned().unwrap_or_default();
            for item in &items {
                let entry = if item["track"].is_object() {
                    &item["track"]
                } else {
                    &item["item"]
                };
                let names: Vec<String> = entry["artists"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| text(&a["name"]))
                    .filter(|n| !n.is_empty())
                    .collect();
                if names.is_empty() {
                    continue;
                }
                tracks += 1;
                for name in names {
                    *counts.entry(name).or_default() += 1;
                }
            }
            if items.len() < 100 || page["next"].is_null() {
                break;
            }
            offset += 100;
        }
        let mut artists: Vec<(String, u32)> = counts.into_iter().collect();
        artists.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        artists.truncate(25);
        Ok((tracks, artists))
    }

    /// Build the owner's playlist index (names plus the artists in each), and save it.
    pub async fn build_index(&self) -> Result<PlaylistIndex> {
        use futures_util::StreamExt;
        let playlists = self.my_playlists().await?;
        let profiles: Vec<IndexedPlaylist> = futures_util::stream::iter(playlists)
            .map(|playlist| async move {
                // Some playlists (for example Spotify's own mixes) cannot be read; their
                // names still count.
                let (tracks, artists) = self
                    .playlist_artists(&playlist.uri)
                    .await
                    .unwrap_or_default();
                IndexedPlaylist {
                    playlist,
                    tracks,
                    artists,
                }
            })
            .buffered(3)
            .collect()
            .await;
        let index = PlaylistIndex {
            built_at: now(),
            playlists: profiles,
        };
        let _ = write_private(&self.dir.join("playlist_index.json"), &index);
        *self.index.lock().expect("spotify index") = Some(index.clone());
        Ok(index)
    }

    /// The saved playlist index, if any; starts a background rebuild when missing or stale.
    pub fn playlist_index(self: &Arc<Self>) -> Option<PlaylistIndex> {
        let current = self.index.lock().expect("spotify index").clone();
        let stale = current
            .as_ref()
            .is_none_or(|i| now().saturating_sub(i.built_at) > INDEX_TTL_SECS);
        if stale
            && !self
                .indexing
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let this = self.clone();
            tokio::spawn(async move {
                match this.build_index().await {
                    Ok(index) => tracing::info!(
                        playlists = index.playlists.len(),
                        "Spotify playlist index built"
                    ),
                    Err(error) => {
                        tracing::warn!(%error, "Spotify playlist index could not be built")
                    }
                }
                this.indexing
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
        current
    }
}

/// The owner's playlists with the artists that fill them, so "my radiohead playlist" can
/// find a playlist whose name never says so.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistIndex {
    /// Unix seconds.
    pub built_at: u64,
    pub playlists: Vec<IndexedPlaylist>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedPlaylist {
    pub playlist: Candidate,
    /// Tracks read for the profile.
    pub tracks: u32,
    /// Most frequent artists first, with how many of those tracks they are on.
    pub artists: Vec<(String, u32)>,
}

/// One playlist's fit for a query.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistMatch {
    pub playlist: Candidate,
    /// How well the name (or description) matches; 0 when it does not.
    pub name_score: usize,
    /// Share of the playlist's tracks by artists matching the query (0.0-1.0).
    pub artist_share: f32,
}

impl PlaylistMatch {
    /// Sure enough to ask about it without the reasoning model.
    pub fn confident(&self) -> bool {
        self.name_score > 0 || self.artist_share >= ARTIST_SHARE_MATCH
    }
}

/// Does the artist name contain every word of the query?
fn artist_matches(query: &str, artist: &str) -> bool {
    let q = words(query);
    let a = words(artist);
    !q.is_empty() && q.iter().all(|w| a.contains(w))
}

/// Rank the owner's playlists for a query: name first, then how much of it is that artist,
/// then the description. Only playlists with some match are returned.
pub fn rank_playlists(query: &str, index: &PlaylistIndex) -> Vec<PlaylistMatch> {
    let mut ranked: Vec<(usize, PlaylistMatch)> = index
        .playlists
        .iter()
        .map(|p| {
            let name_score = score(query, &p.playlist.name);
            let by_artist: u32 = p
                .artists
                .iter()
                .filter(|(name, _)| artist_matches(query, name))
                .map(|(_, n)| *n)
                .sum();
            let artist_share = if p.tracks == 0 {
                0.0
            } else {
                (by_artist as f32 / p.tracks as f32).min(1.0)
            };
            let description = p
                .playlist
                .detail
                .as_deref()
                .map(|d| score(query, d))
                .unwrap_or(0);
            (
                description,
                PlaylistMatch {
                    playlist: p.playlist.clone(),
                    name_score,
                    artist_share,
                },
            )
        })
        .filter(|(d, m)| m.name_score > 0 || m.artist_share > 0.0 || *d > 0)
        .collect();
    ranked.sort_by(|(da, a), (db, b)| {
        b.name_score
            .cmp(&a.name_score)
            .then(b.artist_share.total_cmp(&a.artist_share))
            .then(db.cmp(da))
    });
    ranked.into_iter().map(|(_, m)| m).collect()
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

    fn indexed(name: &str, uri: &str, tracks: u32, artists: &[(&str, u32)]) -> IndexedPlaylist {
        IndexedPlaylist {
            playlist: Candidate {
                kind: Kind::Playlist,
                name: name.into(),
                by: "Pedro Victor".into(),
                uri: uri.into(),
                detail: None,
            },
            tracks,
            artists: artists.iter().map(|(a, n)| (a.to_string(), *n)).collect(),
        }
    }

    #[test]
    fn a_playlist_full_of_an_artist_is_found_without_its_name() {
        let index = PlaylistIndex {
            built_at: 0,
            playlists: vec![
                indexed(
                    "Road trip",
                    "spotify:playlist:a",
                    50,
                    &[("Radiohead", 3), ("Blur", 20)],
                ),
                indexed(
                    "Cabeça de Radio",
                    "spotify:playlist:b",
                    40,
                    &[("Radiohead", 34), ("Thom Yorke", 6)],
                ),
                indexed("Lo-fi beats", "spotify:playlist:c", 90, &[("Nujabes", 12)]),
            ],
        };
        let ranked = rank_playlists("radiohead", &index);
        assert_eq!(ranked[0].playlist.name, "Cabeça de Radio");
        assert!(ranked[0].confident());
        assert!(
            !ranked[1].confident(),
            "3 of 50 tracks is not a Radiohead playlist"
        );
        assert!(rank_playlists("metallica", &index).is_empty());
        // A matching name still comes first.
        let named = PlaylistIndex {
            built_at: 0,
            playlists: vec![
                index.playlists[1].clone(),
                indexed("Radiohead live", "spotify:playlist:d", 10, &[]),
            ],
        };
        assert_eq!(
            rank_playlists("radiohead", &named)[0].playlist.name,
            "Radiohead live"
        );
    }

    #[test]
    fn playlists_are_described_with_their_creator() {
        let c = candidate(
            Kind::Playlist,
            &json!({"name":"Cabeça de Radio","uri":"spotify:playlist:37i9dQZF1DX","owner":{"display_name":"Pedro Victor","id":"pedro"},"description":"","tracks":{"total":42}}),
        )
        .unwrap();
        assert_eq!(
            c.describe(),
            "the playlist 'Cabeça de Radio' by Pedro Victor"
        );
        assert_eq!(c.detail.as_deref(), Some("42 tracks"));
    }

    #[test]
    fn tracks_and_albums_name_their_artists() {
        let t = candidate(Kind::Track, &json!({"name":"How to Disappear Completely","uri":"spotify:track:2rtGaCAeYal","artists":[{"name":"Radiohead"}],"album":{"name":"Kid A"}})).unwrap();
        assert_eq!(
            t.describe(),
            "the song 'How to Disappear Completely' by Radiohead"
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
    async fn play_rejects_non_spotify_uris_before_any_network_call() {
        let dir = std::env::temp_dir().join(format!("sail-spotify-test-{}", random_token(6)));
        let spotify =
            Spotify::with_endpoints(dir.clone(), "http://127.0.0.1:9", "http://127.0.0.1:9");
        let err = spotify
            .play("https://open.spotify.com/playlist/abc")
            .await
            .unwrap_err();
        assert_eq!(err.code, "invalid");
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
