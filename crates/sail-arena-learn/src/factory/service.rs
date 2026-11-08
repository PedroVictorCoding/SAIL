//! The Course Factory as the daemon and HTTP layer see it: one object that owns the
//! factory's stores and turns owner actions into durable records and queued jobs.
//!
//! The HTTP handlers stay thin and never touch a store directly, so every owner action
//! goes through the same checks whether it comes from the web UI, the Android app or a
//! test. Nothing here calls a model: model work only happens in queued jobs.
use super::contracts::{
    AuthoringJob, ConceptGraph, CourseProject, CurriculumPlan, EducationalSource, FragmentLookup,
    IngestionStatus, Invalidation, LessonStage, PlanStatus, PriorityReason, ProjectStatus,
    QueuedJob, SourceRights, ValidationReceipt, sha256_hex,
};
use super::jobs::{JobError, JobQueue, MAX_RECENT, Pending, Recent};
use super::pipeline::{LessonRow, Pipeline, course_id_for, lesson_rows};
use super::settings::{FactorySettings, SettingsError, SettingsStore};
use super::sources::{SourceError, SourceRegistry};
use super::store::StoreError;
use crate::courses::content::Domain;
use serde::{Deserialize, Serialize};
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
    /// Course generation. `None` keeps the library usable when the store can't open.
    pipeline: Option<Arc<Pipeline>>,
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
            pipeline: None,
        }
    }

    /// Enables the course routes over `pipeline` (which shares this service's stores).
    pub fn with_pipeline(mut self, pipeline: Arc<Pipeline>) -> Self {
        self.pipeline = Some(pipeline);
        self
    }

    pub fn pipeline(&self) -> Result<&Arc<Pipeline>, FactoryError> {
        self.pipeline
            .as_ref()
            .ok_or_else(|| FactoryError::Conflict("course generation is unavailable".into()))
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
            let pages = self
                .pipeline
                .as_ref()
                .and_then(|p| p.pdf_progress(&source.source_id, source.revision));
            out.push(SourceView {
                source,
                fragment_count,
                pages_done: pages.map(|p| p.0),
                pages_total: pages.map(|p| p.1),
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
        let Some(pipeline) = &self.pipeline else {
            return Ok(Invalidation::default());
        };
        let affected = self.sources.affected_by(id)?;
        let deps = pipeline.deps();
        deps.store
            .invalidate(&affected, Some(deps.packages.as_ref()))
            .map_err(store_err)
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

fn store_err(e: StoreError) -> FactoryError {
    match e {
        StoreError::Invalid(m) => FactoryError::Invalid(m),
        StoreError::NotFound(m) => FactoryError::NotFound(m),
        StoreError::Conflict(m) => FactoryError::Conflict(m),
        other => FactoryError::Store(other.to_string()),
    }
}

/// Longest course level text, e.g. "Second-year undergraduate".
pub const MAX_LEVEL_CHARS: usize = 120;
/// Most sources one course may draw on.
pub const MAX_COURSE_SOURCES: usize = 16;

/// `POST /factory/courses`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewCourse {
    pub title: String,
    pub domain: Domain,
    #[serde(default)]
    pub level: String,
    pub source_ids: Vec<String>,
}

/// Counts for the course list.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub planned: usize,
    pub published: usize,
    pub needs_review: usize,
    pub failed: usize,
    pub queued_jobs: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CourseSummary {
    pub course: CourseProject,
    pub progress: Progress,
}

#[derive(Debug, Clone, Serialize)]
pub struct CourseDetail {
    pub course: CourseProject,
    pub graph: Option<ConceptGraph>,
    pub plan: Option<CurriculumPlan>,
    pub lessons: Vec<LessonRow>,
}

/// One citation in the draft inspector. Location and chapter only: the owner opens the
/// book for the text.
#[derive(Debug, Clone, Serialize)]
pub struct Citation {
    pub fragment_id: String,
    pub chapter_path: Vec<String>,
    pub location: Option<super::contracts::SourceLocation>,
    pub quality: Option<super::contracts::QualityStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DraftSummary {
    pub draft_id: String,
    pub course_id: String,
    pub lesson_plan_id: String,
    pub stage: LessonStage,
    pub output: serde_json::Value,
    pub output_hash: String,
    pub built_from: Vec<super::contracts::SourceAnchorSet>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DraftDetail {
    pub draft: DraftSummary,
    pub receipts: Vec<ValidationReceipt>,
    pub citations: Vec<Citation>,
}

impl FactoryService {
    fn enqueue_all(&self, follow: Vec<super::jobs::FollowUp>) -> Result<(), FactoryError> {
        let now = (self.clock)();
        for f in follow {
            self.jobs.enqueue(f.job, f.reasons, f.tie_break, now)?;
        }
        Ok(())
    }

    fn project(&self, id: &str) -> Result<CourseProject, FactoryError> {
        self.pipeline()?
            .deps()
            .store
            .project(id)
            .map_err(store_err)?
            .ok_or_else(|| FactoryError::NotFound(format!("course {id}")))
    }

    /// Start a course over library sources. Mapping begins as soon as every source is
    /// extracted (right away for EPUB, Markdown and text).
    pub fn create_course(&self, req: &NewCourse) -> Result<CourseProject, FactoryError> {
        let pipeline = self.pipeline()?;
        let title = req.title.trim();
        if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
            return Err(FactoryError::Invalid(format!(
                "the title must be 1–{MAX_TITLE_CHARS} characters"
            )));
        }
        if req.level.chars().count() > MAX_LEVEL_CHARS {
            return Err(FactoryError::Invalid("the level is too long".into()));
        }
        if req.source_ids.is_empty() || req.source_ids.len() > MAX_COURSE_SOURCES {
            return Err(FactoryError::Invalid(format!(
                "a course needs 1–{MAX_COURSE_SOURCES} sources"
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for id in &req.source_ids {
            if !seen.insert(id) {
                return Err(FactoryError::Invalid(format!(
                    "source {id} is listed twice"
                )));
            }
            if self.sources.source(id).is_none() {
                return Err(FactoryError::NotFound(format!("source {id}")));
            }
        }
        let project = CourseProject {
            course_id: course_id_for(title),
            title: title.into(),
            domain: req.domain,
            source_ids: req.source_ids.clone(),
            // The store wants a level; an owner who left it blank means "no particular level".
            level: match req.level.trim() {
                "" => "General".into(),
                level => level.into(),
            },
            status: ProjectStatus::Extracting,
            created_ms: (self.clock)(),
        };
        let project = pipeline
            .deps()
            .store
            .create_project(project)
            .map_err(store_err)?;
        let follow = pipeline
            .start_mapping(&project.course_id)
            .map_err(FactoryError::Store)?;
        self.enqueue_all(follow)?;
        self.project(&project.course_id)
    }

    pub fn courses(&self) -> Result<Vec<CourseSummary>, FactoryError> {
        let pipeline = self.pipeline()?;
        let store = &pipeline.deps().store;
        let active = self.jobs.active()?;
        let mut out = Vec::new();
        for course in store.projects().map_err(store_err)? {
            let mut progress = Progress {
                queued_jobs: active
                    .iter()
                    .filter(|p| job_course(&p.queued.job) == Some(course.course_id.as_str()))
                    .count(),
                ..Progress::default()
            };
            if let Some(plan) = store.latest_plan(&course.course_id).map_err(store_err)? {
                let drafts = store.drafts_for(&course.course_id).map_err(store_err)?;
                for row in lesson_rows(&plan, &drafts, None) {
                    progress.planned += 1;
                    match row.stage {
                        Some(LessonStage::Published) => progress.published += 1,
                        Some(LessonStage::NeedsReview) => progress.needs_review += 1,
                        Some(LessonStage::Failed) => progress.failed += 1,
                        _ => {}
                    }
                }
            }
            out.push(CourseSummary { course, progress });
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.course.created_ms));
        Ok(out)
    }

    pub fn course(&self, id: &str) -> Result<CourseDetail, FactoryError> {
        let pipeline = self.pipeline()?;
        let deps = pipeline.deps();
        let course = self.project(id)?;
        let graph = deps.store.latest_graph(id).map_err(store_err)?;
        let plan = deps.store.latest_plan(id).map_err(store_err)?;
        let published = deps
            .packages
            .latest(id)
            .map_err(|e| FactoryError::Store(e.to_string()))?
            .map(|r| r.revision);
        let lessons = match &plan {
            Some(plan) => {
                let drafts = deps.store.drafts_for(id).map_err(store_err)?;
                lesson_rows(plan, &drafts, published)
            }
            None => Vec::new(),
        };
        Ok(CourseDetail {
            course,
            graph,
            plan,
            lessons,
        })
    }

    /// The owner approves a proposed plan: lesson generation starts.
    pub fn approve_plan(&self, id: &str, revision: u64) -> Result<CurriculumPlan, FactoryError> {
        let pipeline = self.pipeline()?;
        let store = &pipeline.deps().store;
        let course = self.project(id)?;
        if !matches!(
            course.status,
            ProjectStatus::AwaitingPlanApproval | ProjectStatus::Generating
        ) {
            return Err(FactoryError::Conflict(
                "there is no plan waiting for approval".into(),
            ));
        }
        let plan = store.approve_plan(id, revision).map_err(store_err)?;
        store
            .set_project_status(id, ProjectStatus::Generating)
            .map_err(store_err)?;
        self.enqueue_all(pipeline.lesson_jobs(&plan))?;
        Ok(plan)
    }

    /// Pause one course: its queued jobs wait (nothing is lost). Resume works out where it
    /// was from what is stored.
    pub fn set_course_paused(&self, id: &str, paused: bool) -> Result<CourseProject, FactoryError> {
        let pipeline = self.pipeline()?;
        let store = &pipeline.deps().store;
        let course = self.project(id)?;
        if paused {
            if matches!(
                course.status,
                ProjectStatus::Complete | ProjectStatus::Failed { .. }
            ) {
                return Err(FactoryError::Conflict(
                    "a finished course can't be paused".into(),
                ));
            }
            return store
                .set_project_status(id, ProjectStatus::Paused)
                .map_err(store_err);
        }
        if course.status != ProjectStatus::Paused {
            return Ok(course);
        }
        let plan = store.latest_plan(id).map_err(store_err)?;
        let status = match &plan {
            Some(p) if p.status == PlanStatus::OwnerApproved => ProjectStatus::Generating,
            Some(_) => ProjectStatus::AwaitingPlanApproval,
            None if store.latest_graph(id).map_err(store_err)?.is_some() => ProjectStatus::Mapping,
            None => ProjectStatus::Extracting,
        };
        let restarting = status == ProjectStatus::Extracting;
        let course = store.set_project_status(id, status).map_err(store_err)?;
        if restarting {
            self.enqueue_all(pipeline.start_mapping(id).map_err(FactoryError::Store)?)?;
        }
        Ok(course)
    }

    /// "Generate the next lesson now": the first planned lesson without a usable draft
    /// jumps the queue.
    pub fn next_lesson(&self, id: &str) -> Result<QueuedJob, FactoryError> {
        let pipeline = self.pipeline()?;
        let store = &pipeline.deps().store;
        let course = self.project(id)?;
        if course.status != ProjectStatus::Generating {
            return Err(FactoryError::Conflict(
                "approve the plan first (or resume the course)".into(),
            ));
        }
        let plan = store
            .latest_plan(id)
            .map_err(store_err)?
            .filter(|p| p.status == PlanStatus::OwnerApproved)
            .ok_or_else(|| FactoryError::Conflict("the plan isn't approved".into()))?;
        let drafts = store.drafts_for(id).map_err(store_err)?;
        let next = lesson_rows(&plan, &drafts, None)
            .into_iter()
            .find(|row| {
                row.stage
                    .is_none_or(|s| s.is_terminal() && s != LessonStage::Published)
            })
            .ok_or_else(|| FactoryError::Conflict("every lesson already has a draft".into()))?;
        let job = AuthoringJob::GenerateLesson {
            course_id: id.into(),
            plan_revision: plan.revision,
            lesson_plan_id: next.lesson_plan_id,
        };
        let reasons = vec![PriorityReason::OwnerRequest];
        let now = (self.clock)();
        self.jobs.enqueue(job.clone(), reasons.clone(), now, now)?;
        Ok(QueuedJob {
            job,
            priority: super::jobs::priority(&reasons, now),
            reasons,
        })
    }

    pub fn draft(&self, id: &str) -> Result<DraftDetail, FactoryError> {
        let pipeline = self.pipeline()?;
        let draft = pipeline
            .deps()
            .store
            .draft(id)
            .map_err(store_err)?
            .ok_or_else(|| FactoryError::NotFound(format!("draft {id}")))?;
        let output: serde_json::Value = serde_json::from_str(&draft.raw_json)
            .map_err(|e| FactoryError::Store(e.to_string()))?;
        let mut cited: Vec<String> = draft
            .output
            .claims
            .iter()
            .flat_map(|c| c.fragment_ids.iter().cloned())
            .collect();
        cited.sort();
        cited.dedup();
        let citations = cited
            .into_iter()
            .map(|fragment_id| {
                let f = self.sources.fragment(&fragment_id);
                Citation {
                    chapter_path: f
                        .as_ref()
                        .map(|f| f.chapter_path.clone())
                        .unwrap_or_default(),
                    location: f.as_ref().map(|f| f.location.clone()),
                    quality: f.map(|f| f.quality),
                    fragment_id,
                }
            })
            .collect();
        let receipts = pipeline.receipts(id);
        Ok(DraftDetail {
            draft: DraftSummary {
                draft_id: draft.draft_id,
                course_id: draft.course_id,
                lesson_plan_id: draft.lesson_plan_id,
                stage: draft.stage,
                output,
                output_hash: draft.output_hash,
                built_from: draft.built_from,
            },
            receipts,
            citations,
        })
    }

    /// The owner publishes a draft the checks passed (`ready`) or couldn't fully verify
    /// (`needs_review`). A draft with a failed check can't be published.
    pub fn publish_draft(&self, id: &str) -> Result<(String, u64), FactoryError> {
        let pipeline = self.pipeline()?;
        let deps = pipeline.deps();
        let draft = deps
            .store
            .draft(id)
            .map_err(store_err)?
            .ok_or_else(|| FactoryError::NotFound(format!("draft {id}")))?;
        match draft.stage {
            LessonStage::NeedsReview => {
                deps.store
                    .advance_stage(id, LessonStage::NeedsReview, LessonStage::Ready)
                    .map_err(store_err)?;
            }
            LessonStage::Ready | LessonStage::Published => {}
            other => {
                return Err(FactoryError::Conflict(format!(
                    "a draft in stage {other:?} can't be published"
                )));
            }
        }
        // Publish now rather than through the queue: the owner is waiting, and assembly
        // calls no model.
        let step = pipeline.publish_now(&draft.course_id);
        if let super::jobs::JobOutcome::Failed { reason, .. } = step.outcome {
            return Err(FactoryError::Conflict(reason));
        }
        self.enqueue_all(step.follow_ups)?;
        let revision = deps
            .packages
            .latest(&draft.course_id)
            .map_err(|e| FactoryError::Store(e.to_string()))?
            .map(|r| r.revision)
            .unwrap_or(0);
        Ok((draft.course_id, revision))
    }

    /// The owner rejects a draft; a new one is written. The reason goes to the log only:
    /// it is owner text and never reaches a prompt.
    pub fn reject_draft(&self, id: &str, reason: &str) -> Result<DraftSummary, FactoryError> {
        let pipeline = self.pipeline()?;
        let store = &pipeline.deps().store;
        let draft = store
            .draft(id)
            .map_err(store_err)?
            .ok_or_else(|| FactoryError::NotFound(format!("draft {id}")))?;
        match draft.stage {
            LessonStage::Ready => {}
            // A terminal review can only move to Ready; go through it to fail the draft.
            LessonStage::NeedsReview => {
                store
                    .advance_stage(id, LessonStage::NeedsReview, LessonStage::Ready)
                    .map_err(store_err)?;
            }
            other => {
                return Err(FactoryError::Conflict(format!(
                    "a draft in stage {other:?} can't be rejected"
                )));
            }
        }
        store
            .advance_stage(id, LessonStage::Ready, LessonStage::Failed)
            .map_err(store_err)?;
        tracing::info!(draft = %id, reason = %reason.chars().take(200).collect::<String>(), "course factory: owner rejected a draft");
        let now = (self.clock)();
        self.jobs.enqueue(
            AuthoringJob::GenerateLesson {
                course_id: draft.course_id.clone(),
                plan_revision: draft.plan_revision,
                lesson_plan_id: draft.lesson_plan_id.clone(),
            },
            vec![PriorityReason::OwnerRequest],
            now,
            now,
        )?;
        self.draft(id).map(|d| d.draft)
    }
}

/// The course a job belongs to, if any.
pub fn job_course(job: &AuthoringJob) -> Option<&str> {
    match job {
        AuthoringJob::ExtractConcepts { course_id, .. }
        | AuthoringJob::BuildConceptGraph { course_id }
        | AuthoringJob::BuildCurriculum { course_id, .. }
        | AuthoringJob::GenerateLesson { course_id, .. }
        | AuthoringJob::GenerateExerciseVariant { course_id, .. }
        | AuthoringJob::GenerateReviewAssessment { course_id, .. }
        | AuthoringJob::PublishCourseRevision { course_id } => Some(course_id),
        AuthoringJob::ExtractSource { .. }
        | AuthoringJob::ExtractPdfPages { .. }
        | AuthoringJob::ValidateDraft { .. } => None,
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
