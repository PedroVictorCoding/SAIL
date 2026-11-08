//! The Moodle service: sign-in, the poll loop and one sync's orchestration.
//!
//! A sync reads every enrolled course into a [`CourseSnapshot`], diffs it against the stored
//! one, mirrors new or changed files, and refreshes the course's dated items:
//!
//! - structured Moodle dates and pattern hits are re-derived on every sync (cheap, and an
//!   edited announcement then drops its stale hits);
//! - model extraction runs only for documents it hasn't seen, only when the host admits
//!   background model work, and only while the per-sync model budget lasts. Whatever it
//!   doesn't reach waits for the next sync.
//!
//! Errors are sorted the way [`MoodleError`] intends: `Unauthorized` stops polling until the
//! owner signs in again, `Unavailable` backs off, a function the token can't call skips that
//! feature and keeps the rest.
use crate::client::auth::{launch_url, new_passport, parse_launch_redirect};
use crate::client::rest::RestClient;
use crate::contracts::*;
use crate::credentials::{self, Credentials};
use crate::extract;
use crate::pacer::{PacedApi, is_budget_stop};
use crate::store::Store;
use crate::sync;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Default time between syncs. Course pages change a few times a day at most, and the
/// school's server is shared by thousands of students: be a light client.
pub const DEFAULT_SYNC_INTERVAL: Duration = Duration::hours(3);
/// Shortest interval `$SAIL_MOODLE_SYNC_MINUTES` may set.
pub const MIN_SYNC_MINUTES: i64 = 60;
/// A manual "sync now" is refused within this long of the last attempt.
pub const MANUAL_SYNC_COOLDOWN: Duration = Duration::minutes(10);
/// Courses that ended over two weeks ago are re-read at most this often.
pub const ENDED_COURSE_REFRESH: Duration = Duration::days(7);
/// Grades are re-read at most this often per course.
pub const GRADES_REFRESH: Duration = Duration::hours(24);
/// Longest wait after repeated failures.
pub const MAX_BACKOFF: Duration = Duration::hours(6);
/// How long a launch passport stays valid. The owner signs in within minutes.
pub const LAUNCH_TTL: Duration = Duration::minutes(10);
/// How far back calendar events are read. Recent past events still matter for "what did I
/// miss", and they make the diff's sliding window obvious.
pub const EVENTS_PAST: Duration = Duration::days(7);
/// How far ahead calendar events are read: one semester and a bit.
pub const EVENTS_AHEAD: Duration = Duration::days(200);
/// Most submission-status calls per sync (one per assignment). Older assignments beyond
/// this keep their previous status.
pub const MAX_SUBMISSION_CALLS: usize = 40;
/// Submission status is read only for assignments due in this window around now (or
/// with no due date); others keep their last known status.
pub const SUBMISSION_WINDOW_PAST: Duration = Duration::days(14);
pub const SUBMISSION_WINDOW_AHEAD: Duration = Duration::days(90);
/// Most files downloaded per sync. The first syncs of a semester catch up over several
/// runs instead of pulling everything at once.
pub const MAX_DOWNLOADS_PER_SYNC: usize = 40;
/// Most model calls per sync. Each one is a background inference of up to a few seconds;
/// the rest waits for the next sync rather than hogging the model.
pub const MAX_MODEL_CALLS_PER_SYNC: usize = 40;
/// Most PDF pages read from one file for extraction. Course sheets ("Ficha da UC") and
/// briefs are short; a 400-page textbook is mirrored but only its start is read.
pub const MAX_FILE_TEXT_PAGES: u32 = 40;
/// Largest plain-text/HTML course file read for extraction directly.
pub const MAX_INLINE_TEXT_FILE_BYTES: u64 = 512 * 1024;

/// The URL scheme handed to `launch.php`. Moodle redirects the browser to
/// `sailmoodle://token=…` after SSO; the owner copies that address back to SAIL.
pub const LAUNCH_URL_SCHEME: &str = "sailmoodle";

/// Text of a mirrored file, by content hash. Implemented by the daemon over the
/// `sail-runner` PDF sandbox; `None` in the service means PDFs aren't read.
#[async_trait::async_trait]
pub trait FileText: Send + Sync {
    /// Pages `1..=max_pages` (or fewer) of the PDF stored as `<files>/<sha256>`.
    async fn pdf_pages(
        &self,
        sha256: &str,
        max_pages: u32,
        cancel: CancellationToken,
    ) -> Result<Vec<(u32, String)>, String>;

    /// Whether reading may start now: `Err(reason)` when the owner hasn't allowed sandboxed
    /// PDF reading yet. Checked before every attempt, so background syncs never raise an
    /// approval prompt per file.
    fn ready(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Whether SAIL can read PDF text, for the status page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfReading {
    /// The sandbox runner isn't installed or SAIL can't reach it.
    NotInstalled,
    /// Installed, waiting for the owner's one-time permission.
    NeedsPermission,
    Ready,
}

/// Most PDFs read per sync. Reading is local (sandbox jobs, no Moodle requests), but each
/// file is a few runner jobs; a big first sync catches up over several syncs.
pub const MAX_PDFS_PER_SYNC: usize = 15;

/// Whether background model work may start now. Implemented over Mind's gate.
pub trait AdmitModel: Send + Sync {
    fn admit(&self) -> Result<(), String>;
}

/// Builds the client for stored credentials. Tests swap in a fake Moodle.
pub type ApiFactory =
    Arc<dyn Fn(&Credentials) -> Result<Arc<dyn MoodleApi>, MoodleError> + Send + Sync>;

/// The real client.
pub fn rest_factory() -> ApiFactory {
    Arc::new(|creds: &Credentials| {
        RestClient::new(creds.site.clone(), creds.token.clone())
            .map(|client| Arc::new(client) as Arc<dyn MoodleApi>)
    })
}

/// Optional parts of the service.
#[derive(Clone, Default)]
pub struct ServiceDeps {
    pub model: Option<Arc<dyn ExtractModel>>,
    pub admit: Option<Arc<dyn AdmitModel>>,
    pub file_text: Option<Arc<dyn FileText>>,
    /// Gap between Moodle requests; `None` is [`crate::pacer::MIN_REQUEST_GAP`]. Tests only.
    pub request_gap: Option<std::time::Duration>,
}

/// Why a sign-in attempt failed. Shown to the owner as is.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("{0}")]
    BadInput(String),
    #[error("{0}")]
    Moodle(#[from] MoodleError),
    #[error("{0}")]
    Storage(String),
}

/// The status page.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub configured: bool,
    pub site_url: Option<String>,
    pub site_name: Option<String>,
    pub user_name: Option<String>,
    pub syncing: bool,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub next_sync: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub needs_sign_in: bool,
    /// Features skipped because the token's service doesn't allow them.
    pub unavailable_features: Vec<String>,
    /// Whether file text (PDFs) and model extraction are wired.
    pub reads_pdfs: bool,
    /// Why PDFs are or aren't read.
    pub pdf_reading: PdfReading,
    pub uses_model: bool,
    /// Moodle requests and downloaded bytes today, and the caps.
    pub usage_today: crate::store::Usage,
    pub limits: Limits,
}

/// The conservative-usage caps, shown on the status page.
#[derive(Debug, Clone, Serialize)]
pub struct Limits {
    pub sync_every_minutes: i64,
    pub requests_per_sync: u64,
    pub requests_per_day: u64,
    pub download_bytes_per_day: u64,
    pub manual_sync_cooldown_minutes: i64,
}

/// What one sync did. Logged, returned to tests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    pub courses: usize,
    pub new_changes: usize,
    pub downloaded: usize,
    pub download_failures: usize,
    pub model_calls: usize,
    pub items: usize,
    pub skipped_features: Vec<String>,
}

struct PendingLaunch {
    passport: String,
    site: SiteConfig,
    expires: DateTime<Utc>,
}

#[derive(Default)]
struct Runtime {
    syncing: bool,
    next_sync: Option<DateTime<Utc>>,
    needs_sign_in: bool,
    user_name: Option<String>,
    unavailable: Vec<String>,
    launch: Option<PendingLaunch>,
}

/// One owner's Moodle connection.
pub struct MoodleService {
    dir: PathBuf,
    store: Arc<Store>,
    api: ApiFactory,
    deps: ServiceDeps,
    interval: Duration,
    runtime: Mutex<Runtime>,
    wake: Notify,
    /// Serialises syncs: a manual sync while the loop is mid-sync waits rather than racing.
    sync_lock: tokio::sync::Mutex<()>,
}

static GLOBAL: std::sync::OnceLock<Arc<MoodleService>> = std::sync::OnceLock::new();

/// Register the daemon's service so the model tools can reach it. The first call wins.
pub fn set_global(service: Arc<MoodleService>) {
    let _ = GLOBAL.set(service);
}

/// The daemon's service, when the connector started.
pub fn global() -> Option<Arc<MoodleService>> {
    GLOBAL.get().cloned()
}

/// Where the connector keeps its data: `$SAIL_MOODLE_DIR` or `<data root>/moodle`.
pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SAIL_MOODLE_DIR") {
        return dir.into();
    }
    sail_common::storage::data_dir("moodle")
}

/// The sync interval from `$SAIL_MOODLE_SYNC_MINUTES` (60–1440), else the default.
pub fn interval_from_env() -> Duration {
    std::env::var("SAIL_MOODLE_SYNC_MINUTES")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|m| (MIN_SYNC_MINUTES..=1440).contains(m))
        .map(Duration::minutes)
        .unwrap_or(DEFAULT_SYNC_INTERVAL)
}

/// Wait before the next attempt after `failures` consecutive failures: the interval,
/// doubled per failure, capped at [`MAX_BACKOFF`].
pub fn backoff(interval: Duration, failures: u32) -> Duration {
    let factor = 1i64 << failures.min(10);
    let wait = interval.checked_mul(factor as i32).unwrap_or(MAX_BACKOFF);
    wait.min(MAX_BACKOFF).max(interval.min(MAX_BACKOFF))
}

/// Parse an owner-entered site URL: `https`, a host, no query or fragment; a trailing
/// `/login/...` or `/my/` path is trimmed so pasting any Moodle page works.
pub fn parse_site_url(input: &str) -> Result<SiteConfig, ConnectError> {
    let trimmed = input.trim();
    let mut url = url::Url::parse(trimmed)
        .map_err(|_| ConnectError::BadInput("That isn't a web address.".into()))?;
    let local = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return Err(ConnectError::BadInput(
            "The Moodle address must start with https://".into(),
        ));
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(ConnectError::BadInput(
            "The Moodle address has no host.".into(),
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().to_string();
    let cut = ["/login", "/my", "/course", "/user", "/admin", "/mod"]
        .iter()
        .filter_map(|marker| path.find(marker))
        .min()
        .unwrap_or(path.len());
    let root = path[..cut].trim_end_matches('/').to_string();
    url.set_path(if root.is_empty() { "/" } else { &root });
    Ok(SiteConfig { base_url: url })
}

impl MoodleService {
    /// Open the store under `dir` with the given client factory and optional parts.
    pub fn open(dir: &Path, api: ApiFactory, deps: ServiceDeps) -> Result<Arc<Self>, String> {
        let store = Store::open(dir).map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            dir: dir.to_path_buf(),
            store: Arc::new(store),
            api,
            deps,
            interval: interval_from_env(),
            runtime: Mutex::new(Runtime::default()),
            wake: Notify::new(),
            sync_lock: tokio::sync::Mutex::new(()),
        }))
    }

    /// The store, for read-only HTTP and tool queries.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    fn rt(&self) -> std::sync::MutexGuard<'_, Runtime> {
        self.runtime.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn files_dir(&self) -> PathBuf {
        self.dir.join("files")
    }

    /// Whether PDFs can be read now.
    pub fn pdf_reading(&self) -> PdfReading {
        match &self.deps.file_text {
            None => PdfReading::NotInstalled,
            Some(reader) if reader.ready().is_err() => PdfReading::NeedsPermission,
            Some(_) => PdfReading::Ready,
        }
    }

    /// The status page.
    pub fn status(&self) -> Status {
        let creds = credentials::load(&self.dir);
        let state = self.store.sync_state().unwrap_or_default();
        let rt = self.rt();
        let (configured, site_url, cred_error) = match &creds {
            Ok(Some(c)) => (true, Some(c.site.base_url.to_string()), None),
            Ok(None) => (false, None, None),
            Err(e) => (false, None, Some(e.to_string())),
        };
        Status {
            configured,
            site_url,
            site_name: state.site_name.clone(),
            user_name: rt.user_name.clone(),
            syncing: rt.syncing,
            last_ok: state.last_ok,
            last_attempt: state.last_attempt,
            next_sync: if configured { rt.next_sync } else { None },
            last_error: cred_error.or(state.last_error.clone()),
            needs_sign_in: rt.needs_sign_in || !configured,
            unavailable_features: rt.unavailable.clone(),
            reads_pdfs: self.pdf_reading() == PdfReading::Ready,
            pdf_reading: self.pdf_reading(),
            uses_model: self.deps.model.is_some(),
            usage_today: self.store.usage_today().unwrap_or_default(),
            limits: Limits {
                sync_every_minutes: self.interval.num_minutes(),
                requests_per_sync: crate::pacer::MAX_REQUESTS_PER_SYNC,
                requests_per_day: crate::pacer::MAX_REQUESTS_PER_DAY,
                download_bytes_per_day: crate::pacer::MAX_DOWNLOAD_BYTES_PER_DAY,
                manual_sync_cooldown_minutes: MANUAL_SYNC_COOLDOWN.num_minutes(),
            },
        }
    }

    /// Start an SSO sign-in: a launch URL for the owner's browser. The passport stays here.
    pub fn launch(
        &self,
        site_url: Option<&str>,
    ) -> Result<(url::Url, DateTime<Utc>), ConnectError> {
        let site = parse_site_url(site_url.unwrap_or(DEFAULT_SITE_URL))?;
        let passport = new_passport();
        let mut url = launch_url(&site, &passport, LAUNCH_URL_SCHEME);
        // `confirmed=1` makes Moodle show a page with a "launch the app" link instead of a
        // bare redirect to `sailmoodle://`, which desktop browsers drop silently. The
        // owner copies that link's address. (Moodle keeps the flag across SSO in its
        // `tool_mobile_launch` cookie.)
        url.query_pairs_mut().append_pair("confirmed", "1");
        let expires = Utc::now() + LAUNCH_TTL;
        self.rt().launch = Some(PendingLaunch {
            passport,
            site,
            expires,
        });
        Ok((url, expires))
    }

    /// Finish sign-in with a pasted token or a pasted `sailmoodle://token=…` address. The
    /// token is checked against Moodle (`site_info`) before it's stored.
    pub async fn connect(
        &self,
        site_url: Option<&str>,
        token: Option<&str>,
        redirect: Option<&str>,
    ) -> Result<Status, ConnectError> {
        let (site, token) = match (token, redirect) {
            (Some(token), None) => {
                let site = parse_site_url(site_url.unwrap_or(DEFAULT_SITE_URL))?;
                let token = MoodleToken::new(token.trim());
                if !token.is_plausible() {
                    return Err(ConnectError::BadInput(
                        "A Moodle token is 32 letters and digits (0-9, a-f).".into(),
                    ));
                }
                (site, token)
            }
            (None, Some(redirect)) => {
                let pending = self.rt().launch.take().ok_or_else(|| {
                    ConnectError::BadInput("Start the sign-in again: no sign-in is waiting.".into())
                })?;
                if pending.expires < Utc::now() {
                    return Err(ConnectError::BadInput(
                        "That sign-in expired. Start it again.".into(),
                    ));
                }
                let token = parse_launch_redirect(&pending.site, &pending.passport, redirect)
                    .map_err(|e| {
                        // Keep the passport: the owner may have pasted the wrong line.
                        self.rt().launch = Some(PendingLaunch {
                            passport: pending.passport.clone(),
                            site: pending.site.clone(),
                            expires: pending.expires,
                        });
                        ConnectError::BadInput(e.to_string())
                    })?;
                (pending.site, token)
            }
            _ => {
                return Err(ConnectError::BadInput(
                    "Send either a token or the address Moodle redirected to.".into(),
                ));
            }
        };
        let creds = Credentials {
            site,
            token,
            connected_at: Utc::now(),
        };
        // Even the sign-in check goes through the pacer, so it counts toward the budgets.
        let api = PacedApi::with_gap(
            (self.api)(&creds)?,
            self.store.clone(),
            self.deps
                .request_gap
                .unwrap_or(crate::pacer::MIN_REQUEST_GAP),
        );
        let info = api.site_info().await?;
        credentials::save(&self.dir, &creds).map_err(|e| ConnectError::Storage(e.to_string()))?;
        let mut state = self.store.sync_state().unwrap_or_default();
        state.user_id = Some(info.user_id);
        state.site_name = Some(info.site_name.clone());
        state.last_error = None;
        state.consecutive_failures = 0;
        let _ = self.store.set_sync_state(&state);
        {
            let mut rt = self.rt();
            rt.needs_sign_in = false;
            rt.user_name = Some(info.full_name.clone());
        }
        info!(site = %creds.site.base_url, "Moodle connected");
        self.wake_now();
        Ok(self.status())
    }

    /// Forget the token. Local snapshots, changes and files stay.
    pub fn disconnect(&self) -> Result<(), String> {
        credentials::remove(&self.dir).map_err(|e| e.to_string())?;
        let mut rt = self.rt();
        rt.needs_sign_in = true;
        rt.next_sync = None;
        Ok(())
    }

    /// Ask the loop to sync now. Refused while a sync runs or within
    /// [`MANUAL_SYNC_COOLDOWN`] of the last attempt, so a button can't hammer Moodle.
    pub fn request_sync(&self) -> Result<(), String> {
        if self.rt().syncing {
            return Err("A sync is already running.".into());
        }
        if let Some(last) = self.store.sync_state().ok().and_then(|s| s.last_attempt) {
            let wait = last + MANUAL_SYNC_COOLDOWN - Utc::now();
            if wait > Duration::zero() {
                return Err(format!(
                    "Moodle was checked recently. Try again in {} min.",
                    wait.num_minutes() + 1
                ));
            }
        }
        self.wake.notify_one();
        Ok(())
    }

    /// Wake the loop regardless of the cooldown (used right after connecting).
    fn wake_now(&self) {
        self.wake.notify_one();
    }

    /// Re-read what's already stored (mirrored files, saved snapshots) without contacting
    /// Moodle: used right after the owner allows PDF reading, so files already downloaded
    /// are read now instead of at the next scheduled check. Skipped while a sync runs.
    pub async fn reprocess_local(&self, cancel: CancellationToken) -> Result<SyncReport, String> {
        let Ok(_guard) = self.sync_lock.try_lock() else {
            return Err("A sync is running; it will read the files.".into());
        };
        let mut report = SyncReport::default();
        let mut model_budget = MAX_MODEL_CALLS_PER_SYNC;
        let mut pdf_budget = if self.pdf_reading() == PdfReading::Ready {
            MAX_PDFS_PER_SYNC
        } else {
            0
        };
        for course in self.store.courses().map_err(|e| e.to_string())? {
            let Some(snapshot) = self.store.snapshot(course.id).map_err(|e| e.to_string())? else {
                continue;
            };
            report.courses += 1;
            report.items += self
                .refresh_items(
                    &snapshot,
                    &mut model_budget,
                    &mut pdf_budget,
                    &mut report,
                    &cancel,
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(report)
    }

    /// Start [`Self::reprocess_local`] in the background.
    pub fn start_reprocess(self: &Arc<Self>) {
        let service = self.clone();
        tokio::spawn(async move {
            match service.reprocess_local(CancellationToken::new()).await {
                Ok(report) => info!(?report, "Moodle local re-read done"),
                Err(reason) => info!(%reason, "Moodle local re-read skipped"),
            }
        });
    }

    /// Start the poll loop on the current runtime.
    pub fn start(self: &Arc<Self>, cancel: CancellationToken) {
        let service = self.clone();
        tokio::spawn(async move { service.run(cancel).await });
    }

    /// The poll loop: sync, then sleep until the next due time or a wake-up.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        // Let the daemon finish starting before the first network call.
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_secs(20)) => {}
            _ = self.wake.notified() => {}
        }
        loop {
            let wait = match self.sync_once(cancel.clone()).await {
                Ok(report) => {
                    info!(?report, "Moodle sync done");
                    self.interval
                }
                Err(SyncStop::NotConfigured) | Err(SyncStop::NeedsSignIn) => Duration::hours(24),
                Err(SyncStop::Cancelled) => return,
                Err(SyncStop::Failed(failures)) => backoff(self.interval, failures),
            };
            self.rt().next_sync = Some(Utc::now() + wait);
            let sleep = wait
                .to_std()
                .unwrap_or(std::time::Duration::from_secs(1800));
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(sleep) => {}
                _ = self.wake.notified() => {}
            }
        }
    }

    /// One full sync. Public for the HTTP "sync now" path and tests.
    pub async fn sync_once(&self, cancel: CancellationToken) -> Result<SyncReport, SyncStop> {
        let _guard = self.sync_lock.lock().await;
        let creds = match credentials::load(&self.dir) {
            Ok(Some(creds)) => creds,
            Ok(None) => return Err(SyncStop::NotConfigured),
            Err(e) => {
                self.record_failure(&e.to_string(), false);
                return Err(SyncStop::NotConfigured);
            }
        };
        self.rt().syncing = true;
        let result = self.sync_inner(&creds, cancel).await;
        self.rt().syncing = false;
        match result {
            Ok(report) => {
                let mut state = self.store.sync_state().unwrap_or_default();
                let now = Utc::now();
                state.last_ok = Some(now);
                state.last_attempt = Some(now);
                state.last_error = None;
                state.consecutive_failures = 0;
                let _ = self.store.set_sync_state(&state);
                self.rt().unavailable = report.skipped_features.clone();
                Ok(report)
            }
            Err(MoodleError::Cancelled) => Err(SyncStop::Cancelled),
            Err(MoodleError::Unauthorized(msg)) => {
                self.record_failure(&format!("Sign in to Moodle again ({msg})."), true);
                Err(SyncStop::NeedsSignIn)
            }
            Err(e) => Err(SyncStop::Failed(self.record_failure(&e.to_string(), false))),
        }
    }

    fn record_failure(&self, message: &str, sign_in: bool) -> u32 {
        let mut state = self.store.sync_state().unwrap_or_default();
        state.last_attempt = Some(Utc::now());
        state.last_error = Some(message.to_string());
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let _ = self.store.set_sync_state(&state);
        if sign_in {
            self.rt().needs_sign_in = true;
        }
        warn!(error = %message, "Moodle sync failed");
        state.consecutive_failures
    }

    async fn sync_inner(
        &self,
        creds: &Credentials,
        cancel: CancellationToken,
    ) -> Result<SyncReport, MoodleError> {
        let api: Arc<dyn MoodleApi> = Arc::new(PacedApi::with_gap(
            (self.api)(creds)?,
            self.store.clone(),
            self.deps
                .request_gap
                .unwrap_or(crate::pacer::MIN_REQUEST_GAP),
        ));
        let mut report = SyncReport::default();
        let mut skipped = Skipped::default();
        let info = api.site_info().await?;
        {
            let mut rt = self.rt();
            rt.user_name = Some(info.full_name.clone());
            rt.needs_sign_in = false;
        }
        let mut state = self.store.sync_state().unwrap_or_default();
        let since = state.last_ok.map(|t| t - Duration::days(1));
        state.user_id = Some(info.user_id);
        state.site_name = Some(info.site_name.clone());
        let _ = self.store.set_sync_state(&state);

        let now = Utc::now();
        let mut courses = api.courses(info.user_id).await?;
        courses.truncate(MAX_COURSES);
        let previous = self.store.courses().map_err(store_error)?;
        let mut changes = sync::diff::course_list_diff(&previous, &courses, now);
        let current: HashSet<i64> = courses.iter().map(|c| c.id).collect();
        for gone in previous.iter().filter(|c| !current.contains(&c.id)) {
            let _ = self.store.forget_course(gone.id);
        }
        let ids: Vec<i64> = courses.iter().map(|c| c.id).collect();
        check(&cancel)?;

        let assignments = skipped
            .soft("assignments", api.assignments(&ids).await)?
            .unwrap_or_default();
        let events = skipped
            .soft(
                "calendar",
                api.events(&ids, now - EVENTS_PAST, now + EVENTS_AHEAD)
                    .await,
            )?
            .unwrap_or_default();
        let announcements = skipped
            .soft("announcements", api.announcements(&ids, since).await)?
            .unwrap_or_default();

        let mut by_course: BTreeMap<i64, CourseSnapshot> = BTreeMap::new();
        let mut submission_calls = 0usize;
        for course in &courses {
            check(&cancel)?;
            let previous = self.store.snapshot(course.id).map_err(store_error)?;
            let mut meta = self.store.course_meta(course.id).map_err(store_error)?;
            let ended = course.end.is_some_and(|end| end < now - Duration::days(14));
            let fresh_enough =
                |at: Option<DateTime<Utc>>, every: Duration| at.is_some_and(|at| at > now - every);
            // Ended courses barely change: reuse their snapshot for a week at a time.
            if ended
                && fresh_enough(meta.contents_at, ENDED_COURSE_REFRESH)
                && let Some(prev) = &previous
            {
                by_course.insert(course.id, prev.clone());
                continue;
            }
            let sections = match api.course_contents(course.id).await {
                Ok(sections) => {
                    meta.contents_at = Some(now);
                    sections
                }
                Err(e) if is_budget_stop(&e) => {
                    skipped
                        .0
                        .insert("rest of this sync (request budget)".into());
                    if let Some(prev) = &previous {
                        by_course.insert(course.id, prev.clone());
                    }
                    break;
                }
                Err(e) => return Err(e),
            };
            let course_assignments: Vec<Assignment> = assignments
                .iter()
                .filter(|a| a.course_id == course.id)
                .cloned()
                .collect();
            let mut submissions = Vec::new();
            for assignment in &course_assignments {
                let recent = assignment.due.is_none_or(|due| {
                    due > now - SUBMISSION_WINDOW_PAST && due < now + SUBMISSION_WINDOW_AHEAD
                });
                if !recent || submission_calls >= MAX_SUBMISSION_CALLS {
                    if let Some(old) = previous.as_ref().and_then(|p| {
                        p.submissions
                            .iter()
                            .find(|s| s.assignment_id == assignment.id)
                    }) {
                        submissions.push(old.clone());
                    }
                    continue;
                }
                submission_calls += 1;
                if let Some(status) =
                    skipped.soft("submissions", api.submission_status(assignment.id).await)?
                {
                    submissions.push(status);
                }
            }
            let previous_grades = previous.as_ref().map(|p| p.grades.clone());
            let grades = match previous_grades {
                Some(old) if fresh_enough(meta.grades_at, GRADES_REFRESH) => old,
                old => match skipped.soft("grades", api.grades(course.id, info.user_id).await)? {
                    Some(fresh) => {
                        meta.grades_at = Some(now);
                        fresh
                    }
                    None => old.unwrap_or_default(),
                },
            };
            let _ = self.store.set_course_meta(course.id, &meta);
            // Announcements are read incrementally: keep older ones from the last snapshot.
            let mut course_announcements: Vec<Announcement> = announcements
                .iter()
                .filter(|a| a.course_id == course.id)
                .cloned()
                .collect();
            if let Some(prev) = &previous {
                let fresh: HashSet<i64> = course_announcements
                    .iter()
                    .map(|a| a.discussion_id)
                    .collect();
                course_announcements.extend(
                    prev.announcements
                        .iter()
                        .filter(|a| !fresh.contains(&a.discussion_id))
                        .cloned(),
                );
            }
            course_announcements.sort_by_key(|a| std::cmp::Reverse(a.modified));
            let snapshot = CourseSnapshot {
                course: course.clone(),
                sections,
                assignments: course_assignments,
                submissions,
                events: events
                    .iter()
                    .filter(|e| e.course_id == Some(course.id))
                    .cloned()
                    .collect(),
                announcements: course_announcements,
                grades,
                fetched_at: Utc::now(),
            };
            changes.extend(sync::diff::diff(previous.as_ref(), &snapshot));
            self.store.save_snapshot(&snapshot).map_err(store_error)?;
            by_course.insert(course.id, snapshot);
        }
        report.courses = by_course.len();
        report.new_changes = self.store.record_changes(&changes).map_err(store_error)?;

        if info.can_download_files {
            self.mirror(api.as_ref(), &by_course, &mut report, &cancel)
                .await?;
        } else {
            skipped.0.insert("file downloads".into());
        }

        let mut model_budget = MAX_MODEL_CALLS_PER_SYNC;
        let mut pdf_budget = if self.pdf_reading() == PdfReading::Ready {
            MAX_PDFS_PER_SYNC
        } else {
            0
        };
        for snapshot in by_course.values() {
            check(&cancel)?;
            let items = self
                .refresh_items(
                    snapshot,
                    &mut model_budget,
                    &mut pdf_budget,
                    &mut report,
                    &cancel,
                )
                .await?;
            report.items += items;
        }
        report.skipped_features = skipped.0.into_iter().collect();
        Ok(report)
    }

    async fn mirror(
        &self,
        api: &dyn MoodleApi,
        snapshots: &BTreeMap<i64, CourseSnapshot>,
        report: &mut SyncReport,
        cancel: &CancellationToken,
    ) -> Result<(), MoodleError> {
        let files_dir = self.files_dir();
        let incoming = files_dir.join(".incoming");
        std::fs::create_dir_all(&incoming)
            .map_err(|e| MoodleError::FileRefused(format!("mirror folder: {e}")))?;
        for snapshot in snapshots.values() {
            let ended = snapshot
                .course
                .end
                .is_some_and(|end| end < Utc::now() - Duration::days(14));
            if ended {
                continue;
            }
            let known = self
                .store
                .known_files(snapshot.course.id)
                .map_err(store_error)?;
            let plan = sync::mirror::plan(&snapshot_files(snapshot), &known);
            for file in plan.fetch {
                if report.downloaded + report.download_failures >= MAX_DOWNLOADS_PER_SYNC {
                    return Ok(());
                }
                check(cancel)?;
                let tmp = incoming.join(format!("{}.part", uuid_like()));
                match api.download(&file, &tmp, cancel.clone()).await {
                    Ok(done) => {
                        let Some(blob) = self.store.blob_path(&done.sha256) else {
                            let _ = std::fs::remove_file(&tmp);
                            report.download_failures += 1;
                            continue;
                        };
                        if blob.exists() {
                            let _ = std::fs::remove_file(&tmp);
                        } else if let Err(error) = std::fs::rename(&tmp, &blob) {
                            warn!(%error, "Moodle mirror rename failed");
                            let _ = std::fs::remove_file(&tmp);
                            report.download_failures += 1;
                            continue;
                        }
                        let stored = DownloadedFile { path: blob, ..done };
                        self.store
                            .record_file_version(&file, &stored)
                            .map_err(store_error)?;
                        report.downloaded += 1;
                    }
                    Err(MoodleError::Cancelled) => return Err(MoodleError::Cancelled),
                    Err(e) if is_budget_stop(&e) => {
                        let _ = std::fs::remove_file(&tmp);
                        return Ok(());
                    }
                    Err(MoodleError::Unauthorized(m)) => return Err(MoodleError::Unauthorized(m)),
                    Err(error) => {
                        warn!(%error, file = %file.filename, "Moodle download failed");
                        let _ = std::fs::remove_file(&tmp);
                        report.download_failures += 1;
                    }
                }
            }
        }
        Ok(())
    }

    /// Re-derive a course's items: structured + pattern every time, model for new docs.
    async fn refresh_items(
        &self,
        snapshot: &CourseSnapshot,
        model_budget: &mut usize,
        pdf_budget: &mut usize,
        report: &mut SyncReport,
        cancel: &CancellationToken,
    ) -> Result<usize, MoodleError> {
        let course_id = snapshot.course.id;
        let mut docs = extract::docs::docs_from_snapshot(snapshot);
        docs.extend(self.file_docs(snapshot, pdf_budget, cancel).await?);

        let mut incoming = extract::structured::items_from_snapshot(snapshot);
        let mut assessment = self.store.assessment(course_id).map_err(store_error)?;
        let mut assessment_changed = false;
        for doc in &docs {
            let hits = extract::dates::find_dates(&doc.text, doc.observed_at, doc.term);
            incoming.extend(extract::pattern::items_from_patterns(doc, &hits));
            if self.store.doc_done(&doc.id).map_err(store_error)? {
                continue;
            }
            let (Some(model), true) = (
                self.deps.model.as_ref(),
                extract::prompt::should_ask_model(doc, &hits),
            ) else {
                let _ = self.store.mark_doc(&doc.id, course_id, "skipped");
                continue;
            };
            if *model_budget == 0 {
                continue;
            }
            if let Some(admit) = &self.deps.admit
                && let Err(reason) = admit.admit()
            {
                info!(%reason, "Moodle extraction waits for the model");
                *model_budget = 0;
                continue;
            }
            check(cancel)?;
            *model_budget -= 1;
            report.model_calls += 1;
            let prompt = extract::prompt::build_prompt(doc, &hits);
            match model.complete(&prompt, cancel.child_token()).await {
                Ok(reply) => {
                    let result = extract::validate::parse_reply(&reply, doc);
                    let outcome = if result.items.is_empty() && !result.rejected.is_empty() {
                        "rejected"
                    } else {
                        "done"
                    };
                    let _ = self.store.save_rejected(&result.rejected);
                    incoming.extend(result.items);
                    for component in result.assessment {
                        if !assessment.iter().any(|c| same_component(c, &component)) {
                            assessment.push(component);
                            assessment_changed = true;
                        }
                    }
                    let _ = self.store.mark_doc(&doc.id, course_id, outcome);
                }
                Err(ExtractModelError::Failed(reason)) => {
                    warn!(%reason, doc = %doc.label, "Moodle extraction failed");
                    let _ = self.store.mark_doc(&doc.id, course_id, "rejected");
                }
                Err(other) => {
                    // Preempted, not admitted, unavailable: try again next sync.
                    info!(reason = %other, "Moodle extraction deferred");
                    *model_budget = 0;
                }
            }
        }
        // Model items persist (their docs aren't re-asked); the rest is re-derived. A model
        // item whose page text is gone (edited announcement, changed brief) goes with it;
        // file evidence stays, since file docs are only built once per content hash.
        let live: HashSet<&str> = docs.iter().map(|d| d.id.as_str()).collect();
        let kept: Vec<AcademicItem> = self
            .store
            .items(Some(course_id), None, true)
            .map_err(store_error)?
            .into_iter()
            .filter(|item| item.origin == ItemOrigin::Model)
            .filter_map(|mut item| {
                item.evidence.retain(|e| {
                    live.contains(e.doc_id.as_str()) || matches!(e.source, DocSource::File { .. })
                });
                (!item.evidence.is_empty()).then_some(item)
            })
            .collect();
        let merged = extract::merge::merge(&kept, &incoming);
        self.store
            .replace_items(course_id, &merged)
            .map_err(store_error)?;
        if assessment_changed {
            self.store
                .replace_assessment(course_id, &assessment)
                .map_err(store_error)?;
        }
        Ok(merged.len())
    }

    /// Extraction docs for mirrored PDFs and small text files of this course. A file is
    /// read once per content hash; `file:<sha>` marks it.
    async fn file_docs(
        &self,
        snapshot: &CourseSnapshot,
        pdf_budget: &mut usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<ExtractionDoc>, MoodleError> {
        let mut docs = Vec::new();
        let versions = self
            .store
            .known_files(snapshot.course.id)
            .map_err(store_error)?;
        let files: HashMap<FileKey, FileRef> = snapshot_files(snapshot)
            .into_iter()
            .map(|f| (f.key(), f))
            .collect();
        for (key, version) in versions {
            let Some(file) = files.get(&key) else {
                continue;
            };
            let marker = format!("file:{}", version.sha256);
            if self.store.doc_done(&marker).map_err(store_error)? {
                continue;
            }
            let kind = file_kind(file);
            let pages = match kind {
                FileKind::Pdf => {
                    // Unread PDFs wait (unmarked) until reading is allowed and budget is left.
                    let Some(reader) = &self.deps.file_text else {
                        continue;
                    };
                    if *pdf_budget == 0 || reader.ready().is_err() {
                        continue;
                    }
                    check(cancel)?;
                    *pdf_budget -= 1;
                    match reader
                        .pdf_pages(&version.sha256, MAX_FILE_TEXT_PAGES, cancel.child_token())
                        .await
                    {
                        Ok(pages) => pages,
                        Err(reason) => {
                            // The runner is down or refused: stop PDFs for this sync instead
                            // of failing file after file. A refused file (encrypted,
                            // malformed) would fail again; mark it so it isn't retried.
                            info!(%reason, file = %file.filename, "Moodle PDF text unavailable");
                            let file_refused =
                                reason.starts_with("PDF refused") && !reason.contains("Guardian");
                            if file_refused {
                                let _ =
                                    self.store.mark_doc(&marker, snapshot.course.id, "rejected");
                            } else {
                                *pdf_budget = 0;
                            }
                            continue;
                        }
                    }
                }
                FileKind::Text | FileKind::Html => {
                    match read_small_text(&self.files_dir(), &version.sha256) {
                        Some(text) if kind == FileKind::Html => {
                            vec![(1, extract::text::html_to_text(&text))]
                        }
                        Some(text) => vec![(1, text)],
                        None => continue,
                    }
                }
                FileKind::Other => {
                    let _ = self.store.mark_doc(&marker, snapshot.course.id, "skipped");
                    continue;
                }
            };
            docs.extend(extract::docs::docs_from_file_text(
                &snapshot.course,
                file,
                &version.sha256,
                &pages,
                Utc::now(),
            ));
            let _ = self.store.mark_doc(&marker, snapshot.course.id, "done");
        }
        Ok(docs)
    }
}

/// Why the loop stopped a sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStop {
    NotConfigured,
    NeedsSignIn,
    Cancelled,
    /// A failure; the number is consecutive failures so far.
    Failed(u32),
}

/// Features skipped this sync because the token's service doesn't allow them.
#[derive(Default)]
struct Skipped(std::collections::BTreeSet<String>);

impl Skipped {
    /// `FunctionNotAllowed` becomes `None` and a skipped feature; other errors propagate.
    fn soft<T>(
        &mut self,
        feature: &str,
        result: Result<T, MoodleError>,
    ) -> Result<Option<T>, MoodleError> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(MoodleError::FunctionNotAllowed { .. }) => {
                self.0.insert(feature.to_string());
                Ok(None)
            }
            Err(e) if is_budget_stop(&e) => {
                self.0.insert(format!("{feature} (request budget)"));
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Pdf,
    Text,
    Html,
    Other,
}

fn file_kind(file: &FileRef) -> FileKind {
    let mime = file.mimetype.as_deref().unwrap_or("").to_ascii_lowercase();
    let name = file.filename.to_ascii_lowercase();
    if mime == "application/pdf" || name.ends_with(".pdf") {
        FileKind::Pdf
    } else if mime == "text/html" || name.ends_with(".html") || name.ends_with(".htm") {
        FileKind::Html
    } else if mime.starts_with("text/") || name.ends_with(".txt") || name.ends_with(".md") {
        FileKind::Text
    } else {
        FileKind::Other
    }
}

fn read_small_text(files_dir: &Path, sha256: &str) -> Option<String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let path = files_dir.join(sha256);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INLINE_TEXT_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub use crate::sync::mirror::snapshot_files;

fn same_component(a: &AssessmentComponent, b: &AssessmentComponent) -> bool {
    a.name.trim().eq_ignore_ascii_case(b.name.trim()) && a.weight_percent == b.weight_percent
}

fn check(cancel: &CancellationToken) -> Result<(), MoodleError> {
    if cancel.is_cancelled() {
        Err(MoodleError::Cancelled)
    } else {
        Ok(())
    }
}

fn store_error(e: impl std::fmt::Display) -> MoodleError {
    MoodleError::BadResponse(format!("local store: {e}"))
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let base = Duration::minutes(30);
        assert_eq!(backoff(base, 0), base);
        assert_eq!(backoff(base, 1), Duration::hours(1));
        assert_eq!(backoff(base, 3), Duration::hours(4));
        assert_eq!(backoff(base, 30), MAX_BACKOFF);
    }

    #[test]
    fn site_urls_are_trimmed_to_the_root() {
        let site = parse_site_url("https://moodle26.iscte-iul.pt/login/index.php?x=1").unwrap();
        assert_eq!(site.base_url.as_str(), "https://moodle26.iscte-iul.pt/");
        let site = parse_site_url(" https://school.test/moodle/my/courses.php ").unwrap();
        assert_eq!(site.base_url.as_str(), "https://school.test/moodle");
        assert!(parse_site_url("http://school.test").is_err());
        assert!(parse_site_url("https://user:pw@school.test").is_err());
        assert!(parse_site_url("not a url").is_err());
        assert!(parse_site_url("http://127.0.0.1:8080/").is_ok());
    }

    #[test]
    fn file_kinds() {
        let mut f = FileRef {
            course_id: 1,
            module_id: 1,
            filename: "Ficha UC.PDF".into(),
            filepath: "/".into(),
            size: 1,
            modified: None,
            mimetype: None,
            url: String::new(),
        };
        assert_eq!(file_kind(&f), FileKind::Pdf);
        f.filename = "notes.txt".into();
        assert_eq!(file_kind(&f), FileKind::Text);
        f.filename = "slides.pptx".into();
        assert_eq!(file_kind(&f), FileKind::Other);
    }
}
