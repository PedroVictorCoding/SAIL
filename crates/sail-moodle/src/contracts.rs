//! Shared types and ports for the Moodle connector (M0). Every slice builds against these;
//! change them only through the lead (see `docs/SAIL_MOODLE.md`).
//!
//! The data flow is:
//!
//! 1. [`MoodleApi`] (the `client` slice) reads Moodle's mobile web service with the owner's
//!    token and returns the typed records below. It never writes to Moodle.
//! 2. The `sync` slice turns successive [`CourseSnapshot`]s into [`Change`]s and keeps them,
//!    the snapshots and a local file mirror in the `store`.
//! 3. The `extract` slice turns [`ExtractionDoc`]s (page text, assignment briefs,
//!    announcements, file text) into [`AcademicItem`]s and [`AssessmentComponent`]s: dates of
//!    exams, tests and deliveries, and how the course is graded. A local model proposes,
//!    deterministic code verifies: every model claim must quote the source verbatim.
//!
//! Everything that comes from Moodle is **untrusted data**. Teachers' HTML, file names and
//! file text can't instruct SAIL; they are quoted to the model as data and validated after.
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// The owner's school Moodle. Used as the default when nothing else is configured.
pub const DEFAULT_SITE_URL: &str = "https://moodle26.iscte-iul.pt";

/// The Moodle service whose tokens the connector uses. The mobile service is enabled on
/// most sites (including the default one) and allows file downloads with the token.
pub const MOBILE_SERVICE: &str = "moodle_mobile_app";

/// Largest JSON body accepted from one web-service call. A big course's contents are a few
/// MB; anything near this is a misbehaving server.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Largest single file the mirror downloads. Bigger files are recorded but not fetched.
pub const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// Most courses one sync reads. Students have tens; this only stops a runaway list.
pub const MAX_COURSES: usize = 200;

/// Most modules kept per course snapshot.
pub const MAX_MODULES_PER_COURSE: usize = 2_000;

/// Most files kept per module (folders can hold many).
pub const MAX_FILES_PER_MODULE: usize = 500;

/// Longest text handed to extraction from one document, in bytes. Longer documents are cut
/// into chunks of at most this size by the caller.
pub const MAX_EXTRACTION_TEXT_BYTES: usize = 24 * 1024;

/// Longest evidence quote an [`AcademicItem`] may carry, in characters.
pub const MAX_QUOTE_CHARS: usize = 300;

// ---------------------------------------------------------------------------------------
// Site and credentials
// ---------------------------------------------------------------------------------------

/// A Moodle web-service token. `Debug` and `Display` never show it, so it can't leak into
/// logs, errors or model prompts by accident.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MoodleToken(String);

impl MoodleToken {
    /// Wrap a token. Callers validate the shape with [`MoodleToken::is_plausible`].
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw token, for the one place that builds the request.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Moodle tokens are 32 lowercase hex characters. Anything else is a paste mistake.
    pub fn is_plausible(&self) -> bool {
        self.0.len() == 32 && self.0.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

impl fmt::Debug for MoodleToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MoodleToken(<redacted>)")
    }
}

/// Where the connector talks to. `base_url` is the site root (`https://host[/path]`), not
/// the REST endpoint; the client derives `/webservice/rest/server.php` and
/// `/webservice/pluginfile.php` from it and refuses any other host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteConfig {
    pub base_url: url::Url,
}

/// `core_webservice_get_site_info`, reduced to what SAIL needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteInfo {
    pub site_name: String,
    pub user_id: i64,
    pub full_name: String,
    /// Moodle release string, e.g. `4.5.2 (Build: 20250210)`.
    pub release: String,
    /// Web-service functions this token may call. Empty when the site doesn't list them.
    pub functions: Vec<String>,
    /// Whether the token may download files (`downloadfiles` in site info).
    pub can_download_files: bool,
}

// ---------------------------------------------------------------------------------------
// Course records (all read-only copies of Moodle data)
// ---------------------------------------------------------------------------------------

/// `core_enrol_get_users_courses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Course {
    pub id: i64,
    pub short_name: String,
    pub full_name: String,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub visible: bool,
    /// Moodle's own "last modified" for the course record, when given.
    pub modified: Option<DateTime<Utc>>,
}

/// One section ("topic", "week") of `core_course_get_contents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub id: i64,
    pub number: i64,
    pub name: String,
    /// Raw HTML summary. Untrusted.
    pub summary_html: String,
    pub visible: bool,
    pub modules: Vec<Module>,
}

/// One activity or resource (course module, "cm").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    /// The course-module id (`cmid`).
    pub id: i64,
    /// `resource`, `folder`, `assign`, `quiz`, `forum`, `url`, `page`, `label`, ...
    pub modname: String,
    /// The activity instance id (e.g. the assignment id for `assign`).
    pub instance: i64,
    pub name: String,
    /// Raw HTML description shown on the course page. Untrusted.
    pub description_html: String,
    /// The module's page on Moodle, for the owner to open.
    pub url: Option<String>,
    pub visible: bool,
    /// Dates Moodle shows on the activity (`dates` in contents: opens, due, closes).
    pub dates: Vec<ModuleDate>,
    pub files: Vec<FileRef>,
}

/// A labelled date Moodle attaches to a module (`"Due:"`, `"Opens:"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleDate {
    pub label: String,
    pub at: DateTime<Utc>,
}

/// One downloadable file of a module (`contents[]` with `type == "file"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileRef {
    pub course_id: i64,
    pub module_id: i64,
    pub filename: String,
    /// Folder path inside the module, `/` for top level.
    pub filepath: String,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub mimetype: Option<String>,
    /// The `fileurl` as Moodle gave it, **without** any token.
    pub url: String,
}

impl FileRef {
    /// Identity of a file across snapshots: same module, folder and name.
    pub fn key(&self) -> FileKey {
        FileKey {
            module_id: self.module_id,
            filepath: self.filepath.clone(),
            filename: self.filename.clone(),
        }
    }
}

/// See [`FileRef::key`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileKey {
    pub module_id: i64,
    pub filepath: String,
    pub filename: String,
}

/// `mod_assign_get_assignments`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub id: i64,
    pub module_id: i64,
    pub course_id: i64,
    pub name: String,
    /// Raw HTML brief. Untrusted.
    pub intro_html: String,
    pub opens: Option<DateTime<Utc>>,
    pub due: Option<DateTime<Utc>>,
    pub cutoff: Option<DateTime<Utc>>,
    /// Files attached to the brief (`introattachments`).
    pub attachments: Vec<FileRef>,
}

/// The owner's state on one assignment (`mod_assign_get_submission_status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionStatus {
    pub assignment_id: i64,
    pub state: SubmissionState,
    pub graded: bool,
    /// Grade as Moodle formats it, when released.
    pub grade: Option<String>,
    pub modified: Option<DateTime<Utc>>,
}

/// Moodle's submission states, plus `Unknown` for anything a newer Moodle adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionState {
    New,
    Draft,
    Submitted,
    Reopened,
    Unknown,
}

/// `core_calendar_get_calendar_events` / upcoming view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: i64,
    pub name: String,
    /// Raw HTML. Untrusted.
    pub description_html: String,
    pub course_id: Option<i64>,
    pub module_id: Option<i64>,
    /// `course`, `user`, `site`, `due`, `open`, `close`, ...
    pub event_type: String,
    pub modname: Option<String>,
    pub starts: DateTime<Utc>,
    pub duration_seconds: i64,
    pub location: Option<String>,
}

/// A news-forum ("Avisos") discussion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    pub discussion_id: i64,
    pub course_id: i64,
    pub subject: String,
    /// Raw HTML of the first post. Untrusted.
    pub message_html: String,
    pub author: Option<String>,
    pub posted: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub attachments: Vec<FileRef>,
}

/// One row of the owner's grade report for a course.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GradeItem {
    pub course_id: i64,
    pub item_id: i64,
    pub name: String,
    /// As Moodle formats it (`"15,50"`, `"-"`).
    pub grade: Option<String>,
    pub range: Option<String>,
    pub percentage: Option<String>,
    pub feedback_html: Option<String>,
    pub modified: Option<DateTime<Utc>>,
}

/// Everything read about one course in one sync. The `sync` slice diffs two of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CourseSnapshot {
    pub course: Course,
    pub sections: Vec<Section>,
    pub assignments: Vec<Assignment>,
    pub submissions: Vec<SubmissionStatus>,
    pub events: Vec<CalendarEvent>,
    pub announcements: Vec<Announcement>,
    pub grades: Vec<GradeItem>,
    pub fetched_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------------------
// The client port
// ---------------------------------------------------------------------------------------

/// Why a Moodle call failed, sorted by what the caller should do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoodleError {
    /// The token is missing, expired or revoked (`invalidtoken`, `accessexception`): ask
    /// the owner to sign in again. Never retried.
    #[error("Moodle refused the token: {0}")]
    Unauthorized(String),
    /// The token's service doesn't allow this function. Skip that feature, keep the rest.
    #[error("Moodle doesn't allow {function} for this token")]
    FunctionNotAllowed { function: String },
    /// Network trouble, timeouts, 5xx: retry later with backoff.
    #[error("Moodle is unreachable: {0}")]
    Unavailable(String),
    /// Moodle answered with an exception we don't special-case.
    #[error("Moodle error {code}: {message}")]
    Remote { code: String, message: String },
    /// The reply broke a cap or didn't parse. Not retried until the next sync.
    #[error("Moodle sent an unusable reply: {0}")]
    BadResponse(String),
    /// A download was refused before or during transfer (size, host, redirect).
    #[error("file refused: {0}")]
    FileRefused(String),
    #[error("cancelled")]
    Cancelled,
}

/// A file written to disk by [`MoodleApi::download`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadedFile {
    pub path: PathBuf,
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the bytes written.
    pub sha256: String,
}

/// Read-only access to one Moodle site with one token. The `client` slice implements it
/// over HTTPS; tests use fakes. No method may change anything on Moodle.
#[async_trait::async_trait]
pub trait MoodleApi: Send + Sync {
    async fn site_info(&self) -> Result<SiteInfo, MoodleError>;

    /// Courses the owner is enrolled in (`core_enrol_get_users_courses`).
    async fn courses(&self, user_id: i64) -> Result<Vec<Course>, MoodleError>;

    /// Sections, modules and file lists (`core_course_get_contents`).
    async fn course_contents(&self, course_id: i64) -> Result<Vec<Section>, MoodleError>;

    /// Assignments of these courses (`mod_assign_get_assignments`).
    async fn assignments(&self, course_ids: &[i64]) -> Result<Vec<Assignment>, MoodleError>;

    /// The owner's submission on one assignment (`mod_assign_get_submission_status`).
    async fn submission_status(&self, assignment_id: i64) -> Result<SubmissionStatus, MoodleError>;

    /// Calendar events of these courses between two instants
    /// (`core_calendar_get_calendar_events`).
    async fn events(
        &self,
        course_ids: &[i64],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>, MoodleError>;

    /// News-forum discussions of these courses modified after `since`.
    async fn announcements(
        &self,
        course_ids: &[i64],
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Announcement>, MoodleError>;

    /// The owner's grade items for one course (`gradereport_user_get_grade_items`).
    async fn grades(&self, course_id: i64, user_id: i64) -> Result<Vec<GradeItem>, MoodleError>;

    /// Stream one file into `dest` (a new file; never overwrites), enforcing
    /// [`MAX_FILE_BYTES`], the site host and no redirects. The token is added by the client
    /// and never appears in the returned value or in errors.
    async fn download(
        &self,
        file: &FileRef,
        dest: &std::path::Path,
        cancel: CancellationToken,
    ) -> Result<DownloadedFile, MoodleError>;
}

// ---------------------------------------------------------------------------------------
// Changes (the sync slice's output)
// ---------------------------------------------------------------------------------------

/// What changed between two snapshots of a course.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    CourseAdded,
    CourseRemoved,
    SectionAdded,
    SectionChanged,
    ModuleAdded,
    ModuleRemoved,
    ModuleRenamed,
    ModuleDescriptionChanged,
    ModuleVisibilityChanged,
    FileAdded,
    FileUpdated,
    FileRemoved,
    AssignmentAdded,
    AssignmentDueChanged,
    AssignmentBriefChanged,
    SubmissionChanged,
    EventAdded,
    EventMoved,
    EventRemoved,
    AnnouncementPosted,
    AnnouncementEdited,
    GradeReleased,
    GradeChanged,
}

/// One observed change. `id` is stable for the same change (a hash of course, kind,
/// subject and the new value), so re-running a diff never duplicates rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub id: String,
    pub course_id: i64,
    pub kind: ChangeKind,
    /// Short, human headline: `"New file: Enunciado Projeto.pdf"`.
    pub title: String,
    /// Optional detail: old → new due date, renamed from, etc. Plain text.
    pub detail: Option<String>,
    /// What it's about, for linking and for extraction to pick up.
    pub subject: ChangeSubject,
    pub observed_at: DateTime<Utc>,
}

/// The record a [`Change`] points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChangeSubject {
    Course,
    Section { section_id: i64 },
    Module { module_id: i64 },
    File { key: FileKey },
    Assignment { assignment_id: i64 },
    Event { event_id: i64 },
    Announcement { discussion_id: i64 },
    Grade { item_id: i64 },
}

// ---------------------------------------------------------------------------------------
// Extraction (the extract slice's input and output)
// ---------------------------------------------------------------------------------------

/// Where a piece of extraction text came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocSource {
    SectionSummary {
        section_id: i64,
    },
    ModuleDescription {
        module_id: i64,
    },
    AssignmentBrief {
        assignment_id: i64,
    },
    Announcement {
        discussion_id: i64,
    },
    Event {
        event_id: i64,
    },
    /// Text of a mirrored file; `chunk` is 0-based when a long file is split.
    File {
        key: FileKey,
        sha256: String,
        chunk: u32,
    },
}

/// One plain-text document to extract from. `text` is already converted from HTML/PDF and
/// capped at [`MAX_EXTRACTION_TEXT_BYTES`]. It is untrusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionDoc {
    /// Stable id: a hash of `source` and `text`. Unchanged text is never re-extracted.
    pub id: String,
    pub course_id: i64,
    pub course_name: String,
    pub source: DocSource,
    /// A short label for citing: `"Avisos: Data do 1.º teste"`, `"Ficha da UC.pdf p. 2"`.
    pub label: String,
    pub text: String,
    /// When SAIL read it. Resolves dates without a year ("12 de dezembro").
    pub observed_at: DateTime<Utc>,
    /// The course's start/end, when known, to pick the right academic year.
    pub term: Option<(NaiveDate, NaiveDate)>,
}

/// What kind of dated thing an [`AcademicItem`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Final exam (any "época": normal, recurso, especial).
    Exam,
    /// Mid-term or in-class test ("teste", "frequência", "mini-teste").
    Test,
    /// Moodle quiz or online test window.
    Quiz,
    /// A delivery: assignment, report, project milestone.
    Deadline,
    Presentation,
    /// Lab, practical session that must be attended or prepared.
    Lab,
    /// Class cancelled, moved or replaced.
    ScheduleChange,
    /// Registration or sign-up deadline (exam enrolment, group forming).
    Registration,
    Other,
}

/// When an item happens. `date` is always known; `time` only when the source says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemWhen {
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
    /// For windows (quiz open → close, exam 9:00–11:30).
    pub end_date: Option<NaiveDate>,
    pub end_time: Option<NaiveTime>,
}

/// How an item was found. The order is also trust order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOrigin {
    /// A structured Moodle field (assignment due date, calendar event). Authoritative.
    Moodle,
    /// The deterministic date finder matched a known pattern next to a keyword.
    Pattern,
    /// A model proposed it and the validator grounded it in the text.
    Model,
}

/// How sure SAIL is. Only `Confirmed` items may create reminders without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Conflicting or weakly grounded. Shown, flagged for the owner.
    Uncertain,
    /// Grounded quote and a date that parses from it; not corroborated.
    Likely,
    /// From a structured Moodle field, or two independent sources agree.
    Confirmed,
}

/// The verbatim text an item was taken from. The validator checks that `quote` occurs in
/// the document (after whitespace normalisation) and that the date parses from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub doc_id: String,
    pub source: DocSource,
    pub label: String,
    /// At most [`MAX_QUOTE_CHARS`] characters, copied from the document.
    pub quote: String,
}

/// A dated thing the owner must know about: an exam, a test, a delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcademicItem {
    /// Stable: a hash of course, kind, date and a normalised title. Re-extraction of the
    /// same fact from another document merges into the same id.
    pub id: String,
    pub course_id: i64,
    pub kind: ItemKind,
    /// Plain text, ≤ 120 chars: `"1.º Teste"`, `"Entrega do relatório final"`.
    pub title: String,
    pub when: ItemWhen,
    pub location: Option<String>,
    /// Share of the final grade when the source states it (0–100).
    pub weight_percent: Option<f32>,
    pub origin: ItemOrigin,
    pub confidence: Confidence,
    /// One or more quotes; more than one when sources were merged.
    pub evidence: Vec<Evidence>,
}

/// One component of how a course is graded (from a "Ficha da UC" or the course page).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssessmentComponent {
    pub course_id: i64,
    /// `"Teste 1"`, `"Projeto"`, `"Exame"`.
    pub name: String,
    pub weight_percent: Option<f32>,
    /// Minimum grade required, in the course's scale, when stated (`"9,5 valores"`).
    pub minimum: Option<String>,
    /// `"avaliação contínua"`, `"exame"`, or both, as plain text.
    pub regime: Option<String>,
    pub evidence: Evidence,
}

/// The extract slice's output for one document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub items: Vec<AcademicItem>,
    pub assessment: Vec<AssessmentComponent>,
    /// Model claims dropped by validation, with the reason. Kept for review, never shown
    /// as facts.
    pub rejected: Vec<RejectedClaim>,
}

/// A model claim the validator refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedClaim {
    pub doc_id: String,
    /// The claim as the model sent it, truncated to 500 chars.
    pub claim: String,
    pub reason: String,
}

/// A prompt for the extraction model. Built by the extract slice, sent by the lead's Mind
/// adapter at background priority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractPrompt {
    pub system: String,
    pub user: String,
    pub max_output_tokens: u32,
}

/// Why a model call failed. Mirrors the Course Factory's split: waiting for the owner is
/// not a failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtractModelError {
    /// Foreground work took the model; try again later.
    #[error("preempted: {0}")]
    Preempted(String),
    /// The host refused to start work (memory, swap, busy).
    #[error("not admitted: {0}")]
    NotAdmitted(String),
    /// No model is available.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// The call ran and failed, or the reply was unusable.
    #[error("failed: {0}")]
    Failed(String),
}

/// The local model, as the extract slice sees it. Never an external model: Moodle data is
/// private (vision decision 15).
#[async_trait::async_trait]
pub trait ExtractModel: Send + Sync {
    async fn complete(
        &self,
        prompt: &ExtractPrompt,
        cancel: CancellationToken,
    ) -> Result<String, ExtractModelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_never_prints() {
        let token = MoodleToken::new("0123456789abcdef0123456789abcdef");
        assert!(token.is_plausible());
        assert!(!format!("{token:?}").contains("0123"));
        assert!(!MoodleToken::new("not a token").is_plausible());
    }

    #[test]
    fn confidence_and_origin_order_is_trust_order() {
        assert!(Confidence::Confirmed > Confidence::Likely);
        assert!(Confidence::Likely > Confidence::Uncertain);
        assert!(ItemOrigin::Moodle < ItemOrigin::Model);
    }

    #[test]
    fn file_key_ignores_size_and_date() {
        let a = FileRef {
            course_id: 1,
            module_id: 2,
            filename: "a.pdf".into(),
            filepath: "/".into(),
            size: 10,
            modified: None,
            mimetype: None,
            url: "https://example.test/f".into(),
        };
        let mut b = a.clone();
        b.size = 20;
        assert_eq!(a.key(), b.key());
    }
}
