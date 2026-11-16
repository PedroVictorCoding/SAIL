//! SAIL Pulse: durable local reminders and notification timing.
//!
//! Pulse owns *when* the user should be reminded. It stores reminders in local SQLite under
//! the shared SAIL data root, fires due one-time reminders exactly once, and keeps a missed
//! reminder in `due` until a connected client shows and acknowledges it. A browser tab is a
//! projection of this record, not its owner: closing the page does not cancel or lose a
//! reminder, and restarting the daemon does not replay one that was already acknowledged.
//!
//! The first slice intentionally supports one-time reminders only. Recurrence, calendar
//! sources, external delivery and location triggers are follow-up work (see
//! `docs/SAIL_PULSE.md`).

pub mod attention;
pub mod presence;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Longest reminder text accepted, in characters.
pub const MAX_TEXT_CHARS: usize = 2_000;
/// Longest same-origin link accepted, in bytes.
pub const MAX_LINK_CHARS: usize = 2_048;
/// Longest IANA time-zone name accepted, in bytes.
pub const MAX_TIMEZONE_CHARS: usize = 64;
/// Active reminders returned by one list call.
pub const DEFAULT_LIST_LIMIT: usize = 200;
/// Finished reminders kept in the recent-history view.
pub const DEFAULT_HISTORY_LIMIT: usize = 50;
/// Most reminders fired in one scheduler tick.
const MAX_FIRE_PER_TICK: usize = 256;
/// A clock offset accepted from a client, in minutes (UTC-14:00 .. UTC+14:00).
const MAX_UTC_OFFSET_MINUTES: i32 = 840;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PulseError {
    #[error("reminder {0} not found")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("reminder {id} is {state}; cannot {action}")]
    InvalidState {
        id: String,
        state: ReminderStatus,
        action: &'static str,
    },
    #[error("pulse storage error: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for PulseError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<chrono::ParseError> for PulseError {
    fn from(error: chrono::ParseError) -> Self {
        Self::Invalid(format!("invalid reminder time: {error}"))
    }
}

pub type Result<T> = std::result::Result<T, PulseError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderStatus {
    /// Waiting for its due time.
    Pending,
    /// Due, and waiting for a connected client to show and acknowledge it.
    Due,
    /// A client showed it and acknowledged delivery.
    Delivered,
    /// The user cancelled it before it was delivered.
    Cancelled,
}

impl ReminderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Due => "due",
            Self::Delivered => "delivered",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "pending" => Self::Pending,
            "due" => Self::Due,
            "delivered" => Self::Delivered,
            "cancelled" => Self::Cancelled,
            other => {
                return Err(PulseError::Storage(format!(
                    "unknown reminder state {other}"
                )));
            }
        })
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Due)
    }
}

impl std::fmt::Display for ReminderStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The authoritative record of one reminder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    pub text: String,
    /// Due time in UTC. The time zone below is the user's intended local zone.
    pub due_at: DateTime<Utc>,
    /// IANA time-zone name supplied by the client, e.g. `America/Sao_Paulo`.
    pub timezone: String,
    /// The client's UTC offset at creation, used for quiet-hour decisions.
    pub utc_offset_minutes: i32,
    pub status: ReminderStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the scheduler first moved this reminder from pending to due.
    pub fired_at: Option<DateTime<Utc>>,
    /// When a client acknowledged showing it.
    pub delivered_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub snooze_count: u32,
    /// Optional same-origin destination the learner can open from the reminder.
    /// Relative paths (`/…`) and same-document fragments (`#…`) only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Reminder {
    /// The local time this reminder is due, using the offset recorded at creation.
    pub fn due_local(&self) -> DateTime<chrono::FixedOffset> {
        let offset = chrono::FixedOffset::east_opt(self.utc_offset_minutes * 60)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC offset"));
        self.due_at.with_timezone(&offset)
    }

    pub fn is_active(&self) -> bool {
        self.status.is_active()
    }
}

/// A validated request to create a reminder.
#[derive(Debug, Clone)]
pub struct NewReminder {
    pub text: String,
    pub due_at: DateTime<Utc>,
    pub timezone: String,
    pub utc_offset_minutes: i32,
    pub link: Option<String>,
}

impl NewReminder {
    pub fn new(
        text: impl Into<String>,
        due_at: DateTime<Utc>,
        timezone: impl Into<String>,
        utc_offset_minutes: i32,
    ) -> Result<Self> {
        let text = text.into().trim().to_owned();
        if text.is_empty() {
            return Err(PulseError::Invalid("reminder text is required".into()));
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(PulseError::Invalid(format!(
                "reminder text is longer than {MAX_TEXT_CHARS} characters"
            )));
        }
        let timezone = timezone.into().trim().to_owned();
        validate_timezone(&timezone)?;
        validate_utc_offset(utc_offset_minutes)?;
        Ok(Self {
            text,
            due_at,
            timezone,
            utc_offset_minutes,
            link: None,
        })
    }

    /// Attach a same-origin destination to the reminder. Relative paths and fragments only;
    /// absolute URLs are rejected so a reminder cannot become an open redirect.
    pub fn with_link(mut self, link: Option<impl Into<String>>) -> Result<Self> {
        self.link = match link {
            Some(link) => Some(validate_link(&link.into())?),
            None => None,
        };
        Ok(self)
    }
}

/// Quiet hours and notification preferences. Spoken reminders are opt-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PulseSettings {
    pub quiet_hours_enabled: bool,
    /// Local minute of day (0..1439) when quiet hours start.
    pub quiet_start_minute: u16,
    /// Local minute of day (0..1439) when quiet hours end.
    pub quiet_end_minute: u16,
    /// Off by default: speaking reminders requires a separate explicit opt-in.
    pub spoken_reminders: bool,
    /// When false, due reminders stay in the list but no toast is shown.
    pub in_app_notifications: bool,
    /// IANA zone last reported by the client, used for conversational creation.
    pub timezone: String,
    /// The client's UTC offset in minutes (local minus UTC).
    pub utc_offset_minutes: i32,
}

impl Default for PulseSettings {
    fn default() -> Self {
        Self {
            quiet_hours_enabled: false,
            quiet_start_minute: 22 * 60,
            quiet_end_minute: 7 * 60,
            spoken_reminders: false,
            in_app_notifications: true,
            timezone: "UTC".into(),
            utc_offset_minutes: 0,
        }
    }
}

impl PulseSettings {
    pub fn validate(&self) -> Result<()> {
        if self.quiet_start_minute >= 24 * 60 || self.quiet_end_minute >= 24 * 60 {
            return Err(PulseError::Invalid(
                "quiet hours must be within one day".into(),
            ));
        }
        validate_timezone(&self.timezone)?;
        validate_utc_offset(self.utc_offset_minutes)?;
        Ok(())
    }

    /// Local minute of day for `at`, using the offset recorded in these settings.
    pub fn local_minute(&self, at: DateTime<Utc>) -> u16 {
        let offset = chrono::FixedOffset::east_opt(self.utc_offset_minutes * 60)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC offset"));
        let local = at.with_timezone(&offset);
        (local.hour() * 60 + local.minute()) as u16
    }

    /// True when `at` is inside the configured quiet window. A start equal to the end
    /// means quiet hours are disabled.
    pub fn is_quiet_at(&self, at: DateTime<Utc>) -> bool {
        if !self.quiet_hours_enabled || self.quiet_start_minute == self.quiet_end_minute {
            return false;
        }
        let minute = self.local_minute(at);
        if self.quiet_start_minute < self.quiet_end_minute {
            minute >= self.quiet_start_minute && minute < self.quiet_end_minute
        } else {
            minute >= self.quiet_start_minute || minute < self.quiet_end_minute
        }
    }
}

fn validate_timezone(timezone: &str) -> Result<()> {
    if timezone.is_empty() {
        return Err(PulseError::Invalid("time zone is required".into()));
    }
    if timezone.len() > MAX_TIMEZONE_CHARS
        || !timezone
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_+-:".contains(&b))
    {
        return Err(PulseError::Invalid(
            "time zone must be an IANA name such as America/Sao_Paulo".into(),
        ));
    }
    Ok(())
}

fn validate_utc_offset(minutes: i32) -> Result<()> {
    if !(-MAX_UTC_OFFSET_MINUTES..=MAX_UTC_OFFSET_MINUTES).contains(&minutes) {
        return Err(PulseError::Invalid(
            "UTC offset must be between -840 and 840 minutes".into(),
        ));
    }
    Ok(())
}

/// Only same-origin destinations are accepted: an absolute path (`/learn`, not `//host`) or a
/// same-document fragment (`#arena`). This keeps a reminder's link from becoming an open
/// redirect or a `javascript:`/`data:` URL.
pub(crate) fn validate_link(link: &str) -> Result<String> {
    let link = link.trim().to_owned();
    if link.is_empty() {
        return Err(PulseError::Invalid("reminder link is empty".into()));
    }
    if link.len() > MAX_LINK_CHARS {
        return Err(PulseError::Invalid(format!(
            "reminder link is longer than {MAX_LINK_CHARS} bytes"
        )));
    }
    let same_origin = link.starts_with('/') && !link.starts_with("//") || link.starts_with('#');
    if !same_origin {
        return Err(PulseError::Invalid(
            "reminder link must be a same-origin path or fragment".into(),
        ));
    }
    if link
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\\'))
    {
        return Err(PulseError::Invalid(
            "reminder link contains a disallowed character".into(),
        ));
    }
    Ok(link)
}

fn format_ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Keep in-memory values identical to what SQLite stores (microsecond precision).
fn truncate_micros(at: DateTime<Utc>) -> DateTime<Utc> {
    at.with_nanosecond((at.nanosecond() / 1_000) * 1_000)
        .unwrap_or(at)
}

fn parse_ts(value: Option<String>) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|text| {
            DateTime::parse_from_rfc3339(&text)
                .map(|at| at.with_timezone(&Utc))
                .map_err(PulseError::from)
        })
        .transpose()
}

/// The Pulse store: reminder records, settings, and the event stream of newly due reminders.
pub struct Pulse {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
    events: broadcast::Sender<Reminder>,
}

impl Pulse {
    /// Open a store at `path`, creating its parent directory and table schema when needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| PulseError::Storage(format!("create {}: {e}", parent.display())))?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::init(conn, Some(path.to_path_buf()))
    }

    /// A non-durable store for tests and for running without a writable disk.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Self::init(conn, None)
    }

    /// Open the store at [`default_path`], falling back to memory with a logged error so the
    /// daemon still starts. [`Pulse::is_durable`] then reports `false`.
    pub fn open_default() -> Self {
        let path = std::env::var_os("SAIL_PULSE_DB")
            .map(PathBuf::from)
            .unwrap_or_else(default_path);
        match Self::open(&path) {
            Ok(store) => {
                info!(path = %path.display(), "SAIL Pulse reminder store opened");
                store
            }
            Err(error) => {
                error!(path = %path.display(), %error, "SAIL Pulse store unavailable; reminders will NOT survive a restart");
                Self::open_in_memory().expect("in-memory SQLite opens")
            }
        }
    }

    fn init(conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reminders (
                 id TEXT PRIMARY KEY,
                 text TEXT NOT NULL,
                 due_at TEXT NOT NULL,
                 timezone TEXT NOT NULL,
                 utc_offset_minutes INTEGER NOT NULL,
                 status TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 fired_at TEXT,
                 delivered_at TEXT,
                 cancelled_at TEXT,
                 snooze_count INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS reminders_active_due
                 ON reminders(status, due_at);
             CREATE INDEX IF NOT EXISTS reminders_updated
                 ON reminders(updated_at);
             CREATE TABLE IF NOT EXISTS settings (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 quiet_hours_enabled INTEGER NOT NULL,
                 quiet_start_minute INTEGER NOT NULL,
                 quiet_end_minute INTEGER NOT NULL,
                 spoken_reminders INTEGER NOT NULL,
                 in_app_notifications INTEGER NOT NULL,
                 timezone TEXT NOT NULL,
                 utc_offset_minutes INTEGER NOT NULL,
                 updated_at TEXT NOT NULL
             );",
        )?;
        // Existing stores predate `link`; SQLite has no `ADD COLUMN IF NOT EXISTS`.
        let columns = conn
            .prepare("PRAGMA table_info(reminders)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !columns.iter().any(|name| name == "link") {
            conn.execute("ALTER TABLE reminders ADD COLUMN link TEXT", [])?;
        }
        let now = format_ts(Utc::now());
        conn.execute(
            "INSERT OR IGNORE INTO settings
             (id, quiet_hours_enabled, quiet_start_minute, quiet_end_minute,
              spoken_reminders, in_app_notifications, timezone, utc_offset_minutes, updated_at)
             VALUES (1, 0, 1320, 420, 0, 1, 'UTC', 0, ?1)",
            params![now],
        )?;
        let (events, _) = broadcast::channel(64);
        Ok(Self {
            conn: Mutex::new(conn),
            path,
            events,
        })
    }

    /// True when reminders are stored on disk and survive a restart.
    pub fn is_durable(&self) -> bool {
        self.path.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Newly due reminders, for live clients. A missed event is recovered by fetching the
    /// list: the reminder remains `due` until a client acknowledges it.
    pub fn subscribe(&self) -> broadcast::Receiver<Reminder> {
        self.events.subscribe()
    }

    /// Create a reminder due in the future.
    pub fn create(&self, new: NewReminder) -> Result<Reminder> {
        self.create_at(new, Utc::now())
    }

    /// Create a reminder relative to an explicit clock. Exposed for imports and deterministic
    /// tests; the due time must still be in the future of `now`.
    pub fn create_at(&self, new: NewReminder, now: DateTime<Utc>) -> Result<Reminder> {
        if new.due_at <= now {
            return Err(PulseError::Invalid(
                "reminder time must be in the future".into(),
            ));
        }
        let reminder = Reminder {
            id: Uuid::new_v4().to_string(),
            text: new.text,
            due_at: truncate_micros(new.due_at),
            timezone: new.timezone,
            utc_offset_minutes: new.utc_offset_minutes,
            status: ReminderStatus::Pending,
            created_at: now,
            updated_at: now,
            fired_at: None,
            delivered_at: None,
            cancelled_at: None,
            snooze_count: 0,
            link: new.link,
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO reminders
             (id, text, due_at, timezone, utc_offset_minutes, status, created_at, updated_at,
              fired_at, delivered_at, cancelled_at, snooze_count, link)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, NULL, 0, ?9)",
            params![
                reminder.id.clone(),
                reminder.text.clone(),
                format_ts(reminder.due_at),
                reminder.timezone.clone(),
                reminder.utc_offset_minutes,
                reminder.status.as_str(),
                format_ts(reminder.created_at),
                format_ts(reminder.updated_at),
                reminder.link.clone(),
            ],
        )?;
        Ok(reminder)
    }

    pub fn get(&self, id: &str) -> Result<Reminder> {
        let conn = self.conn.lock().unwrap();
        read_reminder(&conn, id)?.ok_or_else(|| PulseError::NotFound(id.into()))
    }

    /// Reminders still waiting or due, soonest first.
    pub fn list_active(&self, limit: usize) -> Result<Vec<Reminder>> {
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare(
            "SELECT * FROM reminders WHERE status IN ('pending', 'due')
             ORDER BY due_at ASC LIMIT ?1",
        )?;
        let rows = statement.query_map(
            params![limit.clamp(1, DEFAULT_LIST_LIMIT) as i64],
            row_to_reminder,
        )?;
        collect_reminders(rows)
    }

    /// Recently delivered or cancelled reminders, newest first.
    pub fn list_history(&self, limit: usize) -> Result<Vec<Reminder>> {
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare(
            "SELECT * FROM reminders WHERE status IN ('delivered', 'cancelled')
             ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(
            params![limit.clamp(1, DEFAULT_HISTORY_LIMIT) as i64],
            row_to_reminder,
        )?;
        collect_reminders(rows)
    }

    /// All reminders in a bounded window, newest due first.
    pub fn list_all(&self, limit: usize) -> Result<Vec<Reminder>> {
        let conn = self.conn.lock().unwrap();
        let mut statement =
            conn.prepare("SELECT * FROM reminders ORDER BY due_at DESC LIMIT ?1")?;
        let rows = statement.query_map(
            params![limit.clamp(1, DEFAULT_LIST_LIMIT) as i64],
            row_to_reminder,
        )?;
        collect_reminders(rows)
    }

    /// Move every pending reminder whose due time has passed to `due`, unless the user's
    /// quiet hours are active now. Returns only the reminders this call fired, so a repeated
    /// tick cannot notify a client twice. A missed reminder remains `due` until acknowledged.
    pub fn fire_due(&self) -> Result<Vec<Reminder>> {
        self.fire_due_at(Utc::now())
    }

    /// Fire relative to an explicit clock. Exposed for deterministic tests.
    pub fn fire_due_at(&self, now: DateTime<Utc>) -> Result<Vec<Reminder>> {
        let settings = self.settings()?;
        if settings.is_quiet_at(now) {
            return Ok(Vec::new());
        }
        let mut fired = Vec::new();
        let conn = self.conn.lock().unwrap();
        let ids = {
            let mut statement = conn.prepare(
                "SELECT id FROM reminders WHERE status='pending' AND due_at <= ?1
                 ORDER BY due_at ASC LIMIT ?2",
            )?;
            statement
                .query_map(params![format_ts(now), MAX_FIRE_PER_TICK as i64], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in ids {
            let changed = conn.execute(
                "UPDATE reminders SET status='due', fired_at=?1, updated_at=?1
                 WHERE id=?2 AND status='pending'",
                params![format_ts(now), id],
            )?;
            if changed == 1
                && let Some(reminder) = read_reminder(&conn, &id)?
            {
                fired.push(reminder);
            }
        }
        Ok(fired)
    }

    /// A client showed the reminder. Idempotent: acknowledging a delivered reminder returns
    /// it unchanged.
    pub fn acknowledge(&self, id: &str) -> Result<Reminder> {
        let now = Utc::now();
        let conn = self.conn.lock().unwrap();
        let reminder = read_reminder(&conn, id)?.ok_or_else(|| PulseError::NotFound(id.into()))?;
        match reminder.status {
            ReminderStatus::Due => {
                conn.execute(
                    "UPDATE reminders SET status='delivered', delivered_at=?1, updated_at=?1
                     WHERE id=?2 AND status='due'",
                    params![format_ts(now), id],
                )?;
                read_reminder(&conn, id)?.ok_or_else(|| PulseError::NotFound(id.into()))
            }
            ReminderStatus::Delivered => Ok(reminder),
            state => Err(PulseError::InvalidState {
                id: id.into(),
                state,
                action: "acknowledge",
            }),
        }
    }

    /// Cancel a pending or due reminder. Cancelling an already cancelled reminder is
    /// idempotent; a delivered reminder is historical and cannot be cancelled.
    pub fn cancel(&self, id: &str) -> Result<Reminder> {
        let now = Utc::now();
        let conn = self.conn.lock().unwrap();
        let reminder = read_reminder(&conn, id)?.ok_or_else(|| PulseError::NotFound(id.into()))?;
        match reminder.status {
            ReminderStatus::Pending | ReminderStatus::Due => {
                conn.execute(
                    "UPDATE reminders SET status='cancelled', cancelled_at=?1, updated_at=?1
                     WHERE id=?2 AND status IN ('pending', 'due')",
                    params![format_ts(now), id],
                )?;
                read_reminder(&conn, id)?.ok_or_else(|| PulseError::NotFound(id.into()))
            }
            ReminderStatus::Cancelled => Ok(reminder),
            state => Err(PulseError::InvalidState {
                id: id.into(),
                state,
                action: "cancel",
            }),
        }
    }

    /// Move a pending, due, or already delivered reminder to a new future time. It returns to
    /// pending so it is delivered again at the new time.
    pub fn snooze(&self, id: &str, until: DateTime<Utc>) -> Result<Reminder> {
        let now = Utc::now();
        if until <= now {
            return Err(PulseError::Invalid(
                "snooze time must be in the future".into(),
            ));
        }
        let conn = self.conn.lock().unwrap();
        let reminder = read_reminder(&conn, id)?.ok_or_else(|| PulseError::NotFound(id.into()))?;
        let until = truncate_micros(until);
        match reminder.status {
            ReminderStatus::Pending | ReminderStatus::Due | ReminderStatus::Delivered => {
                conn.execute(
                    "UPDATE reminders SET status='pending', due_at=?1, updated_at=?2,
                     fired_at=NULL, delivered_at=NULL, cancelled_at=NULL,
                     snooze_count=snooze_count+1 WHERE id=?3",
                    params![format_ts(until), format_ts(now), id],
                )?;
                read_reminder(&conn, id)?.ok_or_else(|| PulseError::NotFound(id.into()))
            }
            state => Err(PulseError::InvalidState {
                id: id.into(),
                state,
                action: "snooze",
            }),
        }
    }

    /// Start the server-owned scheduler. Each tick fires due reminders once and broadcasts
    /// them to connected clients; the durable `due` state is the fallback for missed events.
    pub fn start_scheduler(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let pulse = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                match pulse.fire_due() {
                    Ok(reminders) => {
                        for reminder in reminders {
                            let _ = pulse.events.send(reminder);
                        }
                    }
                    Err(error) => warn!(%error, "Pulse scheduler could not fire reminders"),
                }
            }
        })
    }

    /// The current preferences.
    pub fn settings(&self) -> Result<PulseSettings> {
        let conn = self.conn.lock().unwrap();
        read_settings(&conn)
    }

    /// Replace the preferences after validating them.
    pub fn update_settings(&self, settings: &PulseSettings) -> Result<PulseSettings> {
        settings.validate()?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE settings SET quiet_hours_enabled=?1, quiet_start_minute=?2,
             quiet_end_minute=?3, spoken_reminders=?4, in_app_notifications=?5,
             timezone=?6, utc_offset_minutes=?7, updated_at=?8 WHERE id=1",
            params![
                settings.quiet_hours_enabled as i64,
                settings.quiet_start_minute as i64,
                settings.quiet_end_minute as i64,
                settings.spoken_reminders as i64,
                settings.in_app_notifications as i64,
                settings.timezone,
                settings.utc_offset_minutes,
                format_ts(Utc::now()),
            ],
        )?;
        read_settings(&conn)
    }
}

/// Default on-disk store under the shared SAIL data root.
pub fn default_path() -> PathBuf {
    sail_common::storage::data_path("pulse").join("pulse.db")
}

static GLOBAL_PULSE: OnceLock<Arc<Pulse>> = OnceLock::new();

/// Make one Pulse store available to model tools. The first installation wins.
pub fn install_global(pulse: Arc<Pulse>) -> bool {
    GLOBAL_PULSE.set(pulse).is_ok()
}

/// The installed Pulse store, if the daemon opened one.
pub fn global() -> Option<Arc<Pulse>> {
    GLOBAL_PULSE.get().cloned()
}

fn read_settings(conn: &Connection) -> Result<PulseSettings> {
    conn.query_row(
        "SELECT quiet_hours_enabled, quiet_start_minute, quiet_end_minute,
                spoken_reminders, in_app_notifications, timezone, utc_offset_minutes
         FROM settings WHERE id=1",
        [],
        |row| {
            Ok(PulseSettings {
                quiet_hours_enabled: row.get::<_, i64>(0)? != 0,
                quiet_start_minute: row.get::<_, i64>(1)? as u16,
                quiet_end_minute: row.get::<_, i64>(2)? as u16,
                spoken_reminders: row.get::<_, i64>(3)? != 0,
                in_app_notifications: row.get::<_, i64>(4)? != 0,
                timezone: row.get(5)?,
                utc_offset_minutes: row.get(6)?,
            })
        },
    )
    .map_err(PulseError::from)
}

fn read_reminder(conn: &Connection, id: &str) -> Result<Option<Reminder>> {
    let row = conn
        .query_row("SELECT * FROM reminders WHERE id=?1", params![id], |row| {
            reminder_from_row(row)
        })
        .optional()?;
    row.transpose()
}

fn row_to_reminder(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Reminder>> {
    reminder_from_row(row)
}

fn collect_reminders(
    rows: rusqlite::MappedRows<
        '_,
        impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Result<Reminder>>,
    >,
) -> Result<Vec<Reminder>> {
    let mut reminders = Vec::new();
    for row in rows {
        reminders.push(row??);
    }
    Ok(reminders)
}

fn reminder_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Reminder>> {
    let status_text: String = row.get("status")?;
    let due_at: String = row.get("due_at")?;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    let fired_at: Option<String> = row.get("fired_at")?;
    let delivered_at: Option<String> = row.get("delivered_at")?;
    let cancelled_at: Option<String> = row.get("cancelled_at")?;
    let result = (|| -> Result<Reminder> {
        Ok(Reminder {
            id: row.get("id")?,
            text: row.get("text")?,
            due_at: DateTime::parse_from_rfc3339(&due_at)?.with_timezone(&Utc),
            timezone: row.get("timezone")?,
            utc_offset_minutes: row.get("utc_offset_minutes")?,
            status: ReminderStatus::parse(&status_text)?,
            created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
            updated_at: DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&Utc),
            fired_at: parse_ts(fired_at)?,
            delivered_at: parse_ts(delivered_at)?,
            cancelled_at: parse_ts(cancelled_at)?,
            snooze_count: row.get::<_, i64>("snooze_count")? as u32,
            link: row.get::<_, Option<String>>("link")?,
        })
    })();
    Ok(result)
}
