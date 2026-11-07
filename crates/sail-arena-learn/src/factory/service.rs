//! The Course Factory as the daemon and HTTP layer see it: one object that owns the
//! factory's stores and turns owner actions into durable records and queued jobs.
//!
//! The HTTP handlers stay thin and never touch a store directly, so every owner action
//! goes through the same checks whether it comes from the web UI, the Android app or a
//! test. Nothing here calls a model: model work only happens in queued jobs.
use super::contracts::{
    AuthoringJob, EducationalSource, IngestionStatus, Invalidation, PriorityReason, SourceRights,
    sha256_hex,
};
use super::jobs::{JobError, JobQueue, MAX_RECENT, Pending, Recent};
use super::settings::{FactorySettings, SettingsError, SettingsStore};
use super::sources::{SourceError, SourceRegistry};
use serde::Serialize;
use std::sync::Arc;

/// Longest scope string the owner may set. Scopes are short labels like `learning:rust`.
pub const MAX_SCOPE_CHARS: usize = 64;
/// Longest title or licence text accepted at import.
pub const MAX_TITLE_CHARS: usize = 300;

#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("{0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    /// Storage trouble. The message is logged; callers show a generic line.
    #[error("factory storage: {0}")]
    Store(String),
}

impl From<SourceError> for FactoryError {
    fn from(e: SourceError) -> Self {
        match e {
            SourceError::Ingest(e) => Self::Invalid(e.to_string()),
            SourceError::Invalid(m) => Self::Invalid(m),
            SourceError::Store(m) => Self::Store(m),
        }
    }
}

impl From<JobError> for FactoryError {
    fn from(e: JobError) -> Self {
        match e {
            JobError::Full => Self::Conflict(e.to_string()),
            JobError::NeedsOwner(_) => Self::Conflict(e.to_string()),
            JobError::Store(m) => Self::Store(m),
        }
    }
}

impl From<SettingsError> for FactoryError {
    fn from(e: SettingsError) -> Self {
        match e {
            SettingsError::Invalid(m) => Self::Invalid(m),
            SettingsError::Store(m) => Self::Store(m),
        }
    }
}

/// What the owner sends with a file.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub file_name: String,
    pub title: Option<String>,
    pub rights: SourceRights,
    pub scope: String,
}

/// Parse the `rights` query value. Unknown text is refused rather than guessed, and the
/// default is the strictest choice: a book is assumed copyrighted unless the owner says so.
pub fn parse_rights(
    rights: Option<&str>,
    license: Option<&str>,
) -> Result<SourceRights, FactoryError> {
    match rights.unwrap_or("restricted") {
        "restricted" => Ok(SourceRights::Restricted),
        "unknown" => Ok(SourceRights::Unknown),
        "owner_private" => Ok(SourceRights::OwnerPrivate),
        "open" => {
            let license = license.map(str::trim).unwrap_or("");
            if license.is_empty() || license.chars().count() > MAX_TITLE_CHARS {
                return Err(FactoryError::Invalid(
                    "an open source needs its licence (an SPDX id such as CC-BY-4.0)".into(),
                ));
            }
            Ok(SourceRights::Open {
                license: license.into(),
            })
        }
        other => Err(FactoryError::Invalid(format!(
            "rights must be restricted, unknown, owner_private or open, not {other:?}"
        ))),
    }
}

/// `learning:<topic>`, lowercase letters, digits, `-`, `_` and `:` only. Scopes name a
/// memory area, so they must stay inside the learning namespace.
pub fn validate_scope(scope: &str) -> Result<(), FactoryError> {
    let ok = scope.starts_with("learning:")
        && scope.len() > "learning:".len()
        && scope.chars().count() <= MAX_SCOPE_CHARS
        && scope
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_:".contains(c));
    if ok {
        Ok(())
    } else {
        Err(FactoryError::Invalid(
            "scope must look like learning:rust (lowercase letters, digits, - _ :)".into(),
        ))
    }
}

/// A source in the library view.
#[derive(Debug, Clone, Serialize)]
pub struct SourceView {
    #[serde(flatten)]
    pub source: EducationalSource,
    pub fragment_count: usize,
    /// Sandboxed PDF progress; `None` when the source isn't extracted page by page.
    pub pages_done: Option<u32>,
    pub pages_total: Option<u32>,
}

/// The queue view: `GET /factory/jobs`.
#[derive(Debug, Clone, Serialize)]
pub struct JobsView {
    pub paused: bool,
    pub running: Option<Pending>,
    pub queue: Vec<Pending>,
    pub recent: Vec<Recent>,
}

/// Clock in epoch milliseconds; injected so tests control time.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct FactoryService {
    sources: Arc<SourceRegistry>,
    jobs: Arc<JobQueue>,
    settings: Arc<SettingsStore>,
    clock: Clock,
}

impl FactoryService {
    pub fn new(
        sources: Arc<SourceRegistry>,
        jobs: Arc<JobQueue>,
        settings: Arc<SettingsStore>,
        clock: Clock,
    ) -> Self {
        Self {
            sources,
            jobs,
            settings,
            clock,
        }
    }

    pub fn jobs(&self) -> &Arc<JobQueue> {
        &self.jobs
    }

    pub fn sources(&self) -> &Arc<SourceRegistry> {
        &self.sources
    }

    /// Import a file. Formats the daemon can parse safely are indexed now; a PDF that needs
    /// the sandbox is stored as `queued` and gets an extraction job. Returns the record and
    /// whether the same bytes were already in the library. Blocking: call it off the async
    /// runtime (`spawn_blocking`), since parsing a large book takes seconds.
    pub fn import(
        &self,
        req: &ImportRequest,
        bytes: &[u8],
    ) -> Result<(EducationalSource, bool), FactoryError> {
        validate_scope(&req.scope)?;
        if req
            .title
            .as_ref()
            .is_some_and(|t| t.trim().is_empty() || t.chars().count() > MAX_TITLE_CHARS)
        {
            return Err(FactoryError::Invalid(format!(
                "the title must be 1–{MAX_TITLE_CHARS} characters"
            )));
        }
        let hash = sha256_hex(bytes);
        let duplicate = self
            .sources
            .list()?
            .iter()
            .any(|s| s.content_hash == hash && s.status != IngestionStatus::Removed);
        let source = self.sources.import(
            &req.file_name,
            bytes,
            req.title.as_deref(),
            None,
            req.rights.clone(),
            &req.scope,
        )?;
        if source.status == IngestionStatus::Queued {
            self.jobs.enqueue(
                AuthoringJob::ExtractSource {
                    source_id: source.source_id.clone(),
                    revision: source.revision,
                },
                vec![PriorityReason::Pipeline],
                source.imported_ms,
                (self.clock)(),
            )?;
        }
        Ok((source, duplicate))
    }

    /// The library, newest first.
    pub fn library(&self) -> Result<Vec<SourceView>, FactoryError> {
        let mut out = Vec::new();
        for source in self.sources.list()? {
            if source.status == IngestionStatus::Removed {
                continue;
            }
            let fragment_count = self.sources.fragments(&source.source_id)?.len();
            out.push(SourceView {
                source,
                fragment_count,
                pages_done: None,
                pages_total: None,
            });
        }
        out.sort_by_key(|a| std::cmp::Reverse(a.source.imported_ms));
        Ok(out)
    }

    /// Remove a source from the library. Its queued extraction stops; history and learner
    /// evidence are kept. The returned [`Invalidation`] lists what depended on it.
    pub fn remove_source(&self, id: &str) -> Result<Invalidation, FactoryError> {
        if self.sources.remove(id)?.is_none() {
            return Err(FactoryError::NotFound(format!("source {id}")));
        }
        self.jobs.cancel_where(
            |job| match job {
                AuthoringJob::ExtractSource { source_id, .. }
                | AuthoringJob::ExtractPdfPages { source_id, .. } => source_id == id,
                _ => false,
            },
            "the source was removed",
        )?;
        Ok(Invalidation::default())
    }

    pub fn jobs_view(&self) -> Result<JobsView, FactoryError> {
        let mut queue = self.jobs.active()?;
        let running = queue
            .iter()
            .position(|p| p.state == sail_continuity::TaskState::Running)
            .map(|i| queue.remove(i));
        Ok(JobsView {
            paused: self.settings.paused()?,
            running,
            queue,
            recent: self.jobs.recent(MAX_RECENT)?,
        })
    }

    pub fn set_paused(&self, paused: bool) -> Result<bool, FactoryError> {
        self.settings.set_paused(paused)?;
        Ok(paused)
    }

    pub fn settings(&self) -> Result<FactorySettings, FactoryError> {
        Ok(self.settings.settings()?)
    }

    pub fn save_settings(&self, s: &FactorySettings) -> Result<FactorySettings, FactoryError> {
        self.settings.save(s)?;
        Ok(*s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sail_continuity::Continuity;

    fn service() -> (FactoryService, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sail-cf-svc-{}", uuid::Uuid::new_v4()));
        let sources = Arc::new(SourceRegistry::open_in_memory(dir.clone()).unwrap());
        let jobs = Arc::new(JobQueue::new(Arc::new(
            Continuity::open_in_memory().unwrap(),
        )));
        let settings = Arc::new(SettingsStore::open_in_memory().unwrap());
        (
            FactoryService::new(sources, jobs, settings, Arc::new(|| 1_000)),
            dir,
        )
    }

    fn req(name: &str) -> ImportRequest {
        ImportRequest {
            file_name: name.into(),
            title: Some("Notes".into()),
            rights: SourceRights::OwnerPrivate,
            scope: "learning:rust".into(),
        }
    }

    #[test]
    fn rights_default_to_restricted_and_open_needs_a_licence() {
        assert_eq!(parse_rights(None, None).unwrap(), SourceRights::Restricted);
        assert!(parse_rights(Some("open"), None).is_err());
        assert!(parse_rights(Some("public"), None).is_err());
        assert_eq!(
            parse_rights(Some("open"), Some("CC-BY-4.0")).unwrap(),
            SourceRights::Open {
                license: "CC-BY-4.0".into()
            }
        );
    }

    #[test]
    fn scopes_stay_in_the_learning_namespace() {
        assert!(validate_scope("learning:rust").is_ok());
        assert!(validate_scope("learning:").is_err());
        assert!(validate_scope("memory:private").is_err());
        assert!(validate_scope("learning:Rust Book").is_err());
    }

    #[test]
    fn markdown_is_indexed_now_and_a_duplicate_is_reported() {
        let (svc, dir) = service();
        let md = b"# Ownership\n\nEach value has one owner.\n\n## Moves\n\nAssignment moves.\n";
        let (src, dup) = svc.import(&req("notes.md"), md).unwrap();
        assert!(!dup);
        assert_eq!(src.status, IngestionStatus::Indexed);
        assert!(svc.jobs().active().unwrap().is_empty(), "nothing to queue");
        let (again, dup) = svc.import(&req("copy.md"), md).unwrap();
        assert!(dup);
        assert_eq!(again.source_id, src.source_id);
        let lib = svc.library().unwrap();
        assert_eq!(lib.len(), 1);
        assert!(lib[0].fragment_count >= 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn removing_a_source_hides_it_and_unknown_ids_are_not_found() {
        let (svc, dir) = service();
        let (src, _) = svc.import(&req("n.md"), b"# A\n\ntext\n").unwrap();
        svc.remove_source(&src.source_id).unwrap();
        assert!(svc.library().unwrap().is_empty());
        assert!(matches!(
            svc.remove_source("nope"),
            Err(FactoryError::NotFound(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_scope_or_title_is_refused_before_import() {
        let (svc, dir) = service();
        let mut r = req("n.md");
        r.scope = "secrets".into();
        assert!(matches!(
            svc.import(&r, b"# A\n"),
            Err(FactoryError::Invalid(_))
        ));
        let mut r = req("n.md");
        r.title = Some("   ".into());
        assert!(svc.import(&r, b"# A\n").is_err());
        assert!(svc.library().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pause_and_settings_round_trip() {
        let (svc, dir) = service();
        assert!(!svc.jobs_view().unwrap().paused);
        svc.set_paused(true).unwrap();
        assert!(svc.jobs_view().unwrap().paused);
        let s = FactorySettings {
            idle_only: false,
            ..FactorySettings::default()
        };
        svc.save_settings(&s).unwrap();
        assert_eq!(svc.settings().unwrap(), s);
        let _ = std::fs::remove_dir_all(dir);
    }
}
