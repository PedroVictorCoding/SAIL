//! The Course Factory pipeline: one handler per [`AuthoringJob`] kind. Each handler does one
//! bounded step, stores its result durably, and returns the jobs that come next.
//!
//! ```text
//! ExtractSource ─▶ ExtractPdfPages × n ─▶ (all sources ready) ─▶ ExtractConcepts × batches
//!   ─▶ BuildConceptGraph ─▶ BuildCurriculum ─▶ owner approves the plan
//!   ─▶ GenerateLesson ─▶ ValidateDraft ─▶ PublishCourseRevision ─▶ CourseEngine
//! ```
//!
//! Every handler is safe to run twice: a job a crash interrupted runs again from its
//! inputs, and each step checks what is already stored before it spends a model call.
//! Source text and model output are data throughout; nothing a fragment or a reply says can
//! change a stage, a receipt or a publish decision.
use super::author::{
    self, AuthorError, ConceptBatchInput,
    assemble::{ConceptBatch, assemble_concept_graph, assemble_curriculum_plan},
    context::{AuthoringContext, ContextBudget},
    prompts::{MAX_BATCH_FRAGMENTS, SCHEMA_LESSON},
    retrieve::FragmentIndex,
};
use super::contracts::{
    AuthorModel, AuthoringJob, ConceptGraph, CourseId, CourseProject, CurriculumPlan,
    EducationalSource, FragmentId, FragmentKind, FragmentLookup, IngestionStatus, LessonDraft,
    LessonStage, ObjectiveSpec, PDF_TEXT_PAGES_PER_CALL, PdfTextError, PdfTextExtractor,
    PlanStatus, PriorityReason, ProjectStatus, QualityStatus, SourceAnchorSet, SourceFragment,
    SourceRights, SourceType, ValidationContext, ValidationReceipt, Verdict,
};
use super::ingest::pdf_text::{PdfCarry, fragments_from_pages_with_carry};
use super::jobs::{FollowUp, JobHandler, JobOutcome, Step};
use super::registry::{PackageRegistry, PublishRequest, Published};
use super::settings::SettingsStore;
use super::sources::SourceRegistry;
use super::store::{FactoryStore, StoreError};
use super::validate::{ReceiptStore, ValidationSuite};
use crate::courses::CourseEngine;
use crate::courses::content::{Chapter, CoursePackage, Lesson, Objective, Step as CourseStep};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Concept batches per course. A 600-page textbook needs a few hundred; beyond this the
/// fragments are sampled evenly so mapping a huge source stays a bounded amount of work.
pub const MAX_CONCEPT_BATCHES: usize = 400;
/// Characters of fragment text per concept batch, on top of the prompt's own text. Keeps a
/// batch inside the local model's window whatever the owner's context setting.
pub const MAX_BATCH_CHARS: usize = 16_000;
/// Drafts per planned lesson before the factory stops regenerating it and asks the owner.
pub const MAX_LESSON_ATTEMPTS: usize = 3;
/// Lessons queued with "ahead of progress" priority when a plan is approved; the rest
/// follow chapter order.
pub const AHEAD_LESSONS: usize = 3;
/// Fragments retrieved per lesson (across all sources of the course).
pub const LESSON_TOP_K: usize = 8;
/// How long to wait before asking a sandbox that was down again.
pub const SANDBOX_RETRY_MS: i64 = 30 * 60_000;

/// What the pipeline needs. Everything is shared with the HTTP service.
pub struct PipelineDeps {
    pub sources: Arc<SourceRegistry>,
    pub store: Arc<FactoryStore>,
    pub packages: Arc<PackageRegistry>,
    pub engine: Arc<CourseEngine>,
    pub settings: Arc<SettingsStore>,
    pub model: Arc<dyn AuthorModel>,
    /// `None` when the sail-runner sandbox isn't configured: modern PDFs then wait for the
    /// owner instead of being parsed in the daemon.
    pub pdf: Option<Arc<dyn PdfTextExtractor>>,
    pub validators: Arc<ValidationSuite>,
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

/// The pipeline's own small state: PDF progress and concept-batch bookkeeping
/// (`learn_factory_pipeline_*`), plus the receipt table, on one connection.
pub struct Pipeline {
    deps: PipelineDeps,
    conn: Mutex<Connection>,
    indexes: Mutex<BTreeMap<(String, u64), Arc<FragmentIndex>>>,
}

/// Why a step stopped. Converted into a [`Step`] at the handler boundary.
struct Stop(JobOutcome);

type R<T> = Result<T, Stop>;

fn fail(reason: impl Into<String>, retryable: bool) -> Stop {
    Stop(JobOutcome::Failed {
        reason: reason.into(),
        retryable,
    })
}

fn store_fail(what: &str) -> impl FnOnce(StoreError) -> Stop + '_ {
    move |e| match e {
        StoreError::Conflict(m) | StoreError::Invalid(m) => fail(format!("{what}: {m}"), false),
        other => fail(format!("{what}: {other}"), true),
    }
}

fn plain_fail<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> Stop + '_ {
    move |e| fail(format!("{what}: {e}"), true)
}

fn author_fail(e: AuthorError) -> Stop {
    match e {
        AuthorError::Model(m) => Stop(JobOutcome::from_model_error(&m)),
        AuthorError::Parse(p) => fail(format!("the model's reply didn't parse: {p}"), true),
        AuthorError::InvalidRequest(m) => fail(m, false),
    }
}

fn pipeline(job: AuthoringJob, tie_break: i64) -> FollowUp {
    FollowUp {
        job,
        reasons: vec![PriorityReason::Pipeline],
        tie_break,
    }
}

fn anchor_set(source: &EducationalSource) -> SourceAnchorSet {
    SourceAnchorSet {
        source_id: source.source_id.clone(),
        source_revision: source.revision,
        content_hash: source.content_hash.clone(),
    }
}

/// The lesson plan ids of a plan, in teaching order, with their unit index.
fn plan_lessons(plan: &CurriculumPlan) -> Vec<(usize, String, String)> {
    plan.units
        .iter()
        .enumerate()
        .flat_map(|(u, unit)| {
            unit.lessons
                .iter()
                .map(move |l| (u, l.id.clone(), l.title.clone()))
        })
        .collect()
}

/// Ids that are unique across every course the engine serves: generated lessons and
/// reviews carry their course id.
pub fn published_lesson_id(course_id: &str, lesson_plan_id: &str) -> String {
    format!("{course_id}::{lesson_plan_id}")
}

/// The strictest rights among `sources`: anything restricted or unknown keeps the whole
/// course restricted.
pub fn combined_rights(sources: &[EducationalSource]) -> SourceRights {
    if sources
        .iter()
        .any(|s| matches!(s.rights, SourceRights::Restricted | SourceRights::Unknown))
        || sources.is_empty()
    {
        return SourceRights::Restricted;
    }
    if sources
        .iter()
        .any(|s| matches!(s.rights, SourceRights::OwnerPrivate))
    {
        return SourceRights::OwnerPrivate;
    }
    let licenses: BTreeSet<String> = sources
        .iter()
        .filter_map(|s| match &s.rights {
            SourceRights::Open { license } => Some(license.clone()),
            _ => None,
        })
        .collect();
    SourceRights::Open {
        license: licenses.into_iter().collect::<Vec<_>>().join(" AND "),
    }
}

impl Pipeline {
    pub fn open(db: &Path, deps: PipelineDeps) -> Result<Self, String> {
        Self::new(Connection::open(db).map_err(|e| e.to_string())?, deps)
    }

    pub fn new(conn: Connection, deps: PipelineDeps) -> Result<Self, String> {
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS learn_factory_pipeline_pdf (
                 source_id   TEXT NOT NULL,
                 revision    INTEGER NOT NULL,
                 pages_total INTEGER NOT NULL,
                 carry       TEXT NOT NULL,
                 PRIMARY KEY (source_id, revision)
             );
             CREATE TABLE IF NOT EXISTS learn_factory_pipeline_batches (
                 course_id      TEXT NOT NULL,
                 batch_key      TEXT NOT NULL,
                 first_fragment TEXT NOT NULL,
                 fragment_ids   TEXT NOT NULL,
                 done           INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (course_id, batch_key)
             );",
        )
        .map_err(|e| e.to_string())?;
        ReceiptStore::init_tables(&conn).map_err(|e| e.to_string())?;
        Ok(Self {
            deps,
            conn: Mutex::new(conn),
            indexes: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn deps(&self) -> &PipelineDeps {
        &self.deps
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn now(&self) -> i64 {
        (self.deps.clock)()
    }

    // ------------------------------------------------------------------ queries for the UI

    /// `(pages_done, pages_total)` of a sandboxed PDF extraction, if one is running or ran.
    pub fn pdf_progress(&self, source_id: &str, revision: u64) -> Option<(u32, u32)> {
        let row: Option<(i64, String)> = self
            .conn()
            .query_row(
                "SELECT pages_total, carry FROM learn_factory_pipeline_pdf
                 WHERE source_id = ?1 AND revision = ?2",
                params![source_id, revision as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (total, carry) = row?;
        let carry: PdfCarry = serde_json::from_str(&carry).ok()?;
        Some((carry.last_page, u32::try_from(total).ok()?))
    }

    /// Every receipt stored for a draft.
    pub fn receipts(&self, draft_id: &str) -> Vec<ValidationReceipt> {
        ReceiptStore::for_draft(&self.conn(), draft_id).unwrap_or_default()
    }

    // ------------------------------------------------------------------- PDF extraction

    async fn extract_source(&self, source_id: &str, revision: u64) -> R<Vec<FollowUp>> {
        let Some(source) = self
            .deps
            .sources
            .get(source_id)
            .map_err(plain_fail("read source"))?
        else {
            return Ok(vec![]);
        };
        if source.revision != revision || source.status != IngestionStatus::Queued {
            // Replaced, removed, or already handled: nothing to do for this revision.
            return Ok(vec![]);
        }
        let Some(pdf) = &self.deps.pdf else {
            self.set_source_status(
                &source,
                IngestionStatus::NeedsReview {
                    reason: "This PDF needs the text sandbox (sail-runner with poppler), \
                             which isn't configured."
                        .into(),
                },
            )?;
            return Ok(self.sources_changed(source_id));
        };
        let total = match pdf.page_count(&source.content_hash).await {
            Ok(n) => n,
            Err(e) => return self.pdf_error(&source, e).map(|_| vec![]),
        };
        if total == 0 || total as usize > super::ingest::MAX_PAGES {
            self.set_source_status(
                &source,
                IngestionStatus::Failed {
                    reason: format!("the PDF reports {total} pages"),
                },
            )?;
            return Ok(self.sources_changed(source_id));
        }
        self.conn()
            .execute(
                "INSERT OR IGNORE INTO learn_factory_pipeline_pdf
                     (source_id, revision, pages_total, carry) VALUES (?1, ?2, ?3, ?4)",
                params![
                    source_id,
                    revision as i64,
                    i64::from(total),
                    serde_json::to_string(&PdfCarry::default()).unwrap_or_default()
                ],
            )
            .map_err(plain_fail("save PDF progress"))?;
        self.set_source_status(&source, IngestionStatus::Extracting)?;
        Ok(vec![pipeline(
            AuthoringJob::ExtractPdfPages {
                source_id: source_id.into(),
                revision,
                first_page: 1,
            },
            source.imported_ms,
        )])
    }

    fn pdf_error(&self, source: &EducationalSource, e: PdfTextError) -> R<()> {
        match e {
            PdfTextError::Unavailable(why) => Err(Stop(JobOutcome::Defer {
                reason: format!("PDF sandbox unavailable: {why}"),
                retry_after_ms: SANDBOX_RETRY_MS,
            })),
            PdfTextError::Refused(why) => {
                self.set_source_status(source, IngestionStatus::Failed { reason: why })?;
                Ok(())
            }
            PdfTextError::Failed(why) => Err(fail(format!("PDF text: {why}"), true)),
        }
    }

    fn set_source_status(&self, source: &EducationalSource, status: IngestionStatus) -> R<()> {
        self.deps
            .sources
            .set_status(&source.source_id, source.revision, status)
            .map_err(plain_fail("update source status"))
    }

    async fn extract_pdf_pages(
        &self,
        source_id: &str,
        revision: u64,
        first: u32,
        cancel: CancellationToken,
    ) -> R<Vec<FollowUp>> {
        let Some(source) = self
            .deps
            .sources
            .get(source_id)
            .map_err(plain_fail("read source"))?
        else {
            return Ok(vec![]);
        };
        if source.revision != revision || source.status != IngestionStatus::Extracting {
            return Ok(vec![]);
        }
        let row: Option<(i64, String)> = self
            .conn()
            .query_row(
                "SELECT pages_total, carry FROM learn_factory_pipeline_pdf
                 WHERE source_id = ?1 AND revision = ?2",
                params![source_id, revision as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(plain_fail("read PDF progress"))?;
        let Some((total, carry)) = row else {
            return Err(fail("PDF progress is missing; re-import the file", false));
        };
        let total = u32::try_from(total).map_err(plain_fail("PDF page count"))?;
        let mut carry: PdfCarry =
            serde_json::from_str(&carry).map_err(plain_fail("PDF progress"))?;
        if carry.last_page + 1 != first {
            // A stale duplicate of a chunk that already ran (or one that ran ahead).
            return Ok(vec![]);
        }
        let Some(pdf) = &self.deps.pdf else {
            return Err(Stop(JobOutcome::Defer {
                reason: "PDF sandbox not configured".into(),
                retry_after_ms: SANDBOX_RETRY_MS,
            }));
        };
        let last = (first + PDF_TEXT_PAGES_PER_CALL - 1).min(total);
        let pages = match pdf.pages(&source.content_hash, first, last, cancel).await {
            Ok(p) => p,
            Err(e) => return self.pdf_error(&source, e).map(|_| vec![]),
        };
        let fragments = fragments_from_pages_with_carry(source_id, revision, &pages, &mut carry)
            .map_err(|e| fail(format!("pages {first}–{last}: {e}"), false))?;
        self.deps
            .sources
            .append_fragments(source_id, revision, &fragments)
            .map_err(plain_fail("store fragments"))?;
        // The carry is saved only after the fragments are durable, so a crash in between
        // replays this chunk, and the idempotent append makes that harmless.
        self.conn()
            .execute(
                "UPDATE learn_factory_pipeline_pdf SET carry = ?3
                 WHERE source_id = ?1 AND revision = ?2",
                params![
                    source_id,
                    revision as i64,
                    serde_json::to_string(&carry).map_err(plain_fail("PDF progress"))?
                ],
            )
            .map_err(plain_fail("save PDF progress"))?;
        if last < total {
            return Ok(vec![pipeline(
                AuthoringJob::ExtractPdfPages {
                    source_id: source_id.into(),
                    revision,
                    first_page: last + 1,
                },
                source.imported_ms,
            )]);
        }
        let unreadable = self
            .deps
            .sources
            .fragments(source_id)
            .map_err(plain_fail("read fragments"))?
            .iter()
            .filter(|f| matches!(f.quality, QualityStatus::NeedsValidation { .. }))
            .count();
        let status = if unreadable > 0 {
            IngestionStatus::NeedsReview {
                reason: format!("{unreadable} pages have no text layer; OCR isn't available"),
            }
        } else {
            IngestionStatus::Indexed
        };
        self.set_source_status(&source, status)?;
        Ok(self.sources_changed(source_id))
    }

    /// A source finished (or failed) extraction: start mapping every project waiting on it.
    fn sources_changed(&self, source_id: &str) -> Vec<FollowUp> {
        let Ok(projects) = self.deps.store.projects() else {
            return vec![];
        };
        projects
            .into_iter()
            .filter(|p| {
                p.status == ProjectStatus::Extracting && p.source_ids.iter().any(|s| s == source_id)
            })
            .flat_map(|p| self.start_mapping(&p.course_id).unwrap_or_default())
            .collect()
    }

    // ---------------------------------------------------------------------- concept map

    fn usable_source(source: &EducationalSource) -> bool {
        matches!(
            source.status,
            IngestionStatus::Indexed | IngestionStatus::NeedsReview { .. }
        )
    }

    /// When every source of a project is extracted, plan its concept batches and return
    /// their jobs. Called on project creation and whenever a source finishes. Idempotent.
    pub fn start_mapping(&self, course_id: &str) -> Result<Vec<FollowUp>, String> {
        let store = &self.deps.store;
        let Some(project) = store.project(course_id).map_err(|e| e.to_string())? else {
            return Err(format!("no course {course_id}"));
        };
        if project.status != ProjectStatus::Extracting {
            return Ok(vec![]);
        }
        let mut sources = Vec::new();
        for id in &project.source_ids {
            match self.deps.sources.get(id).map_err(|e| e.to_string())? {
                Some(s) if Self::usable_source(&s) => sources.push(s),
                Some(s)
                    if matches!(
                        s.status,
                        IngestionStatus::Failed { .. } | IngestionStatus::Removed
                    ) =>
                {
                    store
                        .set_project_status(
                            course_id,
                            ProjectStatus::Failed {
                                reason: format!("source \"{}\" couldn't be read", s.title),
                            },
                        )
                        .map_err(|e| e.to_string())?;
                    return Ok(vec![]);
                }
                Some(_) => return Ok(vec![]), // still extracting
                None => return Err(format!("unknown source {id}")),
            }
        }
        let budget = (self.context_chars() / 2).min(MAX_BATCH_CHARS);
        let mut batches: Vec<Vec<FragmentId>> = Vec::new();
        let mut current: Vec<FragmentId> = Vec::new();
        let mut chars = 0;
        for source in &sources {
            for f in self
                .deps
                .sources
                .fragments(&source.source_id)
                .map_err(|e| e.to_string())?
            {
                if matches!(
                    f.kind,
                    FragmentKind::Heading | FragmentKind::Bibliography | FragmentKind::Figure
                ) || matches!(f.quality, QualityStatus::NeedsValidation { .. })
                {
                    continue;
                }
                let len = f.text.chars().count().min(MAX_BATCH_CHARS);
                if !current.is_empty()
                    && (current.len() >= MAX_BATCH_FRAGMENTS || chars + len > budget)
                {
                    batches.push(std::mem::take(&mut current));
                    chars = 0;
                }
                current.push(f.fragment_id.clone());
                chars += len;
            }
        }
        if !current.is_empty() {
            batches.push(current);
        }
        if batches.is_empty() {
            store
                .set_project_status(
                    course_id,
                    ProjectStatus::Failed {
                        reason: "the sources have no usable text".into(),
                    },
                )
                .map_err(|e| e.to_string())?;
            return Ok(vec![]);
        }
        if batches.len() > MAX_CONCEPT_BATCHES {
            // Even sampling keeps coverage of the whole book instead of only its start.
            let step = batches.len() as f64 / MAX_CONCEPT_BATCHES as f64;
            batches = (0..MAX_CONCEPT_BATCHES)
                .map(|i| batches[(i as f64 * step) as usize].clone())
                .collect();
        }
        {
            let conn = self.conn();
            for (i, ids) in batches.iter().enumerate() {
                conn.execute(
                    "INSERT OR IGNORE INTO learn_factory_pipeline_batches
                         (course_id, batch_key, first_fragment, fragment_ids)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        course_id,
                        format!("{i:05}"),
                        ids[0],
                        serde_json::to_string(ids).map_err(|e| e.to_string())?
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        store
            .set_project_status(course_id, ProjectStatus::Mapping)
            .map_err(|e| e.to_string())?;
        Ok(batches
            .into_iter()
            .enumerate()
            .map(|(i, fragment_ids)| {
                pipeline(
                    AuthoringJob::ExtractConcepts {
                        course_id: course_id.into(),
                        fragment_ids,
                    },
                    i as i64,
                )
            })
            .collect())
    }

    fn context_chars(&self) -> usize {
        let tokens = self
            .deps
            .settings
            .settings()
            .map(|s| s.context_tokens)
            .unwrap_or(6_000) as usize;
        ContextBudget::new(tokens).max_chars()
    }

    fn batches_left(&self, course_id: &str) -> R<i64> {
        self.conn()
            .query_row(
                "SELECT COUNT(*) FROM learn_factory_pipeline_batches
                 WHERE course_id = ?1 AND done = 0",
                params![course_id],
                |r| r.get(0),
            )
            .map_err(plain_fail("count concept batches"))
    }

    async fn extract_concepts(
        &self,
        course_id: &str,
        fragment_ids: &[FragmentId],
        cancel: CancellationToken,
    ) -> R<Vec<FollowUp>> {
        let first = fragment_ids
            .first()
            .ok_or_else(|| fail("empty concept batch", false))?;
        let row: Option<(String, i64)> = self
            .conn()
            .query_row(
                "SELECT batch_key, done FROM learn_factory_pipeline_batches
                 WHERE course_id = ?1 AND first_fragment = ?2",
                params![course_id, first],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(plain_fail("read concept batch"))?;
        let Some((batch_key, done)) = row else {
            return Ok(vec![]); // a batch of an older plan
        };
        if done == 0 {
            let fragments: Vec<SourceFragment> = fragment_ids
                .iter()
                .filter_map(|id| self.deps.sources.fragment(id))
                .collect();
            if fragments.is_empty() {
                return Err(fail("the batch's fragments no longer resolve", false));
            }
            let parsed = author::extract_concepts(
                self.deps.model.as_ref(),
                &ConceptBatchInput {
                    course_id,
                    fragments: &fragments,
                },
                cancel,
            )
            .await
            .map_err(author_fail)?;
            let in_prompt: Vec<FragmentId> =
                fragments.iter().map(|f| f.fragment_id.clone()).collect();
            self.deps
                .store
                .save_concept_batch(
                    course_id,
                    &batch_key,
                    &parsed.value,
                    &parsed.raw_json,
                    &in_prompt,
                )
                .map_err(store_fail("save concept batch"))?;
            self.conn()
                .execute(
                    "UPDATE learn_factory_pipeline_batches SET done = 1
                     WHERE course_id = ?1 AND batch_key = ?2",
                    params![course_id, batch_key],
                )
                .map_err(plain_fail("mark concept batch"))?;
        }
        if self.batches_left(course_id)? == 0 {
            return Ok(vec![pipeline(
                AuthoringJob::BuildConceptGraph {
                    course_id: course_id.into(),
                },
                0,
            )]);
        }
        Ok(vec![])
    }

    fn project(&self, course_id: &str) -> R<CourseProject> {
        self.deps
            .store
            .project(course_id)
            .map_err(store_fail("read course"))?
            .ok_or_else(|| fail(format!("no course {course_id}"), false))
    }

    fn set_project(&self, course_id: &str, status: ProjectStatus) -> R<()> {
        self.deps
            .store
            .set_project_status(course_id, status)
            .map(|_| ())
            .map_err(store_fail("update course"))
    }

    fn build_graph(&self, course_id: &str) -> R<Vec<FollowUp>> {
        let project = self.project(course_id)?;
        if project.status != ProjectStatus::Mapping {
            return Ok(vec![]);
        }
        let batches: Vec<ConceptBatch> = self
            .deps
            .store
            .concept_batches(course_id)
            .map_err(store_fail("read concept batches"))?;
        let revision = self
            .deps
            .store
            .latest_graph(course_id)
            .map_err(store_fail("read graph"))?
            .map_or(1, |g| g.revision + 1);
        let (graph, _report) =
            match assemble_concept_graph(course_id, revision, &batches, self.deps.sources.as_ref())
            {
                Ok(g) => g,
                Err(e) => {
                    self.set_project(
                        course_id,
                        ProjectStatus::Failed {
                            reason: format!("the concept map couldn't be built: {e}"),
                        },
                    )?;
                    return Ok(vec![]);
                }
            };
        if graph.concepts.is_empty() {
            self.set_project(
                course_id,
                ProjectStatus::Failed {
                    reason: "no concepts were found in the sources".into(),
                },
            )?;
            return Ok(vec![]);
        }
        let graph = self
            .deps
            .store
            .save_graph(graph)
            .map_err(store_fail("save graph"))?;
        Ok(vec![pipeline(
            AuthoringJob::BuildCurriculum {
                course_id: course_id.into(),
                concept_graph_revision: graph.revision,
            },
            0,
        )])
    }

    async fn build_curriculum(
        &self,
        course_id: &str,
        graph_revision: u64,
        cancel: CancellationToken,
    ) -> R<Vec<FollowUp>> {
        let project = self.project(course_id)?;
        if !matches!(
            project.status,
            ProjectStatus::Mapping | ProjectStatus::AwaitingPlanApproval
        ) {
            return Ok(vec![]);
        }
        if let Some(plan) = self
            .deps
            .store
            .latest_plan(course_id)
            .map_err(store_fail("read plan"))?
            && plan.concept_graph_revision == graph_revision
        {
            // This graph already has a plan.
            return Ok(vec![]);
        }
        let graph = self
            .deps
            .store
            .graph(course_id, graph_revision)
            .map_err(store_fail("read graph"))?
            .ok_or_else(|| fail("the concept graph revision is gone", false))?;
        let parsed = author::plan_curriculum(self.deps.model.as_ref(), &graph, cancel)
            .await
            .map_err(author_fail)?;
        let revision = self
            .deps
            .store
            .latest_plan(course_id)
            .map_err(store_fail("read plan"))?
            .map_or(1, |p| p.revision + 1);
        let plan =
            assemble_curriculum_plan(&graph, parsed.value, course_id, revision, project.domain)
                .map_err(|e| fail(format!("the proposed plan was rejected: {e}"), true))?;
        self.deps
            .store
            .save_plan(plan)
            .map_err(store_fail("save plan"))?;
        self.set_project(course_id, ProjectStatus::AwaitingPlanApproval)?;
        Ok(vec![])
    }

    /// Jobs for every lesson of an approved plan that has no usable draft yet. The first
    /// [`AHEAD_LESSONS`] go first; the rest follow chapter order.
    pub fn lesson_jobs(&self, plan: &CurriculumPlan) -> Vec<FollowUp> {
        let mut out = Vec::new();
        let mut ahead = 0;
        for (order, (unit, lesson_id, _)) in plan_lessons(plan).into_iter().enumerate() {
            let latest = self
                .deps
                .store
                .latest_draft_for(&plan.course_id, &lesson_id)
                .ok()
                .flatten();
            if latest.is_some_and(|d| d.stage == LessonStage::Published || !d.stage.is_terminal()) {
                continue;
            }
            let reason = if ahead < AHEAD_LESSONS {
                ahead += 1;
                PriorityReason::AheadOfProgress {
                    lesson_plan_id: lesson_id.clone(),
                    distance: ahead as u32,
                }
            } else {
                PriorityReason::RemainingChapter {
                    unit_index: unit as u32,
                }
            };
            out.push(FollowUp {
                job: AuthoringJob::GenerateLesson {
                    course_id: plan.course_id.clone(),
                    plan_revision: plan.revision,
                    lesson_plan_id: lesson_id,
                },
                reasons: vec![reason],
                tie_break: order as i64,
            });
        }
        out
    }

    // ------------------------------------------------------------------------- lessons

    fn index_for(&self, source: &EducationalSource) -> R<Arc<FragmentIndex>> {
        let key = (source.source_id.clone(), source.revision);
        if let Some(i) = self
            .indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
        {
            return Ok(i.clone());
        }
        let fragments = self
            .deps
            .sources
            .fragments(&source.source_id)
            .map_err(plain_fail("read fragments"))?;
        let index = Arc::new(FragmentIndex::build(
            &source.source_id,
            source.revision,
            &fragments,
        ));
        let mut cache = self.indexes.lock().unwrap_or_else(|p| p.into_inner());
        // Old revisions of the same source are never asked for again.
        cache.retain(|(id, _), _| id != &source.source_id);
        cache.insert(key, index.clone());
        Ok(index)
    }

    async fn generate_lesson(
        &self,
        course_id: &str,
        plan_revision: u64,
        lesson_plan_id: &str,
        cancel: CancellationToken,
    ) -> R<Vec<FollowUp>> {
        let project = self.project(course_id)?;
        if project.status != ProjectStatus::Generating {
            return Ok(vec![]);
        }
        let store = &self.deps.store;
        let plan = store
            .plan(course_id, plan_revision)
            .map_err(store_fail("read plan"))?
            .ok_or_else(|| fail("the plan revision is gone", false))?;
        if plan.status != PlanStatus::OwnerApproved {
            return Ok(vec![]);
        }
        let drafts: Vec<LessonDraft> = store
            .drafts_for(course_id)
            .map_err(store_fail("read drafts"))?
            .into_iter()
            .filter(|d| d.lesson_plan_id == lesson_plan_id)
            .collect();
        if drafts
            .iter()
            .any(|d| d.stage == LessonStage::Published || !d.stage.is_terminal())
        {
            return Ok(vec![]);
        }
        if drafts.len() >= MAX_LESSON_ATTEMPTS
            && drafts.iter().all(|d| d.stage == LessonStage::Failed)
        {
            return Err(fail(
                format!("{MAX_LESSON_ATTEMPTS} drafts of this lesson failed the checks"),
                false,
            ));
        }
        let lesson_plan = plan
            .units
            .iter()
            .flat_map(|u| u.lessons.iter())
            .find(|l| l.id == lesson_plan_id)
            .ok_or_else(|| fail("the lesson isn't in the plan", false))?;
        let graph = store
            .graph(course_id, plan.concept_graph_revision)
            .map_err(store_fail("read graph"))?
            .ok_or_else(|| fail("the plan's concept graph is gone", false))?;
        let objective_id = lesson_plan
            .objective_ids
            .first()
            .ok_or_else(|| fail("the lesson has no objective", false))?;
        let (objective, prerequisites, families) = objective_context(&graph, objective_id)
            .ok_or_else(|| {
                fail(
                    format!("objective {objective_id} isn't in the graph"),
                    false,
                )
            })?;

        let mut sources = Vec::new();
        for id in &project.source_ids {
            if let Some(s) = self.deps.sources.source(id) {
                sources.push(s);
            }
        }
        let query = format!("{} {}", lesson_plan.title, objective.statement);
        let mut hits = Vec::new();
        for source in &sources {
            hits.extend(self.index_for(source)?.search(&query, LESSON_TOP_K));
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        let fragments: Vec<SourceFragment> = hits
            .into_iter()
            .take(LESSON_TOP_K)
            .map(|h| h.fragment)
            .collect();
        if fragments.is_empty() {
            return Err(fail("no source passage matches this lesson", false));
        }
        let related: Vec<String> = plan
            .units
            .iter()
            .find(|u| u.lessons.iter().any(|l| l.id == lesson_plan_id))
            .map(|u| {
                u.lessons
                    .iter()
                    .filter(|l| l.id != lesson_plan_id)
                    .map(|l| l.title.clone())
                    .collect()
            })
            .unwrap_or_default();
        let tokens = self
            .deps
            .settings
            .settings()
            .map(|s| s.context_tokens)
            .unwrap_or(6_000) as usize;
        let ctx = author::context::build(
            &AuthoringContext {
                objective: &objective,
                prerequisites: &prerequisites,
                fragments: &fragments,
                related_lessons: &related,
                question_families: &families,
                learner_evidence: "",
                schema: SCHEMA_LESSON,
                validation_criteria: &[],
            },
            ContextBudget::new(tokens),
        )
        .map_err(|e| {
            fail(
                format!("{e}; raise the context setting or shorten the objective"),
                false,
            )
        })?;
        let parsed = author::author_lesson(
            self.deps.model.as_ref(),
            &plan,
            lesson_plan_id,
            &ctx,
            cancel,
        )
        .await
        .map_err(author_fail)?;
        let included: BTreeSet<&str> = fragments
            .iter()
            .filter(|f| ctx.included.contains(&f.fragment_id))
            .map(|f| f.source_id.as_str())
            .collect();
        let built_from: Vec<SourceAnchorSet> = sources
            .iter()
            .filter(|s| included.contains(s.source_id.as_str()))
            .map(anchor_set)
            .collect();
        let draft = LessonDraft {
            draft_id: uuid::Uuid::new_v4().to_string(),
            course_id: course_id.into(),
            plan_revision,
            lesson_plan_id: lesson_plan_id.into(),
            output: parsed.value,
            raw_json: parsed.raw_json,
            output_hash: parsed.output_hash,
            built_from,
            stage: LessonStage::DraftGenerated,
            created_ms: self.now(),
        };
        store.save_draft(&draft).map_err(store_fail("save draft"))?;
        Ok(vec![pipeline(
            AuthoringJob::ValidateDraft {
                draft_id: draft.draft_id,
            },
            0,
        )])
    }

    async fn validate_draft(&self, draft_id: &str) -> R<Vec<FollowUp>> {
        let store = &self.deps.store;
        let Some(draft) = store.draft(draft_id).map_err(store_fail("read draft"))? else {
            return Ok(vec![]);
        };
        if draft.stage != LessonStage::DraftGenerated {
            return Ok(vec![]);
        }
        let ctx = ValidationContext {
            fragments: self.deps.sources.as_ref(),
            now_ms: self.now(),
        };
        let receipts = self.deps.validators.validate_all(&draft, &ctx).await;
        if let Some(r) = receipts
            .iter()
            .find(|r| matches!(r.verdict, Verdict::Unavailable { .. }))
        {
            // Not stored: an unavailable receipt would block this exact draft forever.
            let reason = match &r.verdict {
                Verdict::Unavailable { reason } => reason.clone(),
                _ => String::new(),
            };
            return Err(Stop(JobOutcome::Defer {
                reason: format!("a check couldn't run: {reason}"),
                retry_after_ms: SANDBOX_RETRY_MS,
            }));
        }
        ReceiptStore::save_receipts(&self.conn(), &receipts)
            .map_err(plain_fail("save receipts"))?;
        let failed = receipts
            .iter()
            .any(|r| matches!(r.verdict, Verdict::Fail { .. }));
        let review = receipts
            .iter()
            .any(|r| matches!(r.verdict, Verdict::NeedsReview { .. }));
        let next = if failed {
            LessonStage::Failed
        } else if review {
            LessonStage::NeedsReview
        } else if super::validate::publishable(
            &receipts,
            &draft,
            super::contracts::Stakes::Practice,
        )
        .is_ok()
        {
            LessonStage::Ready
        } else {
            LessonStage::Failed
        };
        store
            .advance_stage(draft_id, LessonStage::DraftGenerated, next)
            .map_err(store_fail("advance draft"))?;
        Ok(match next {
            LessonStage::Ready => vec![pipeline(
                AuthoringJob::PublishCourseRevision {
                    course_id: draft.course_id,
                },
                0,
            )],
            // Write it again: the next attempt is a fresh draft. The attempt cap stops it.
            LessonStage::Failed => vec![FollowUp {
                job: AuthoringJob::GenerateLesson {
                    course_id: draft.course_id,
                    plan_revision: draft.plan_revision,
                    lesson_plan_id: draft.lesson_plan_id,
                },
                reasons: vec![PriorityReason::Pipeline],
                tie_break: 0,
            }],
            _ => vec![],
        })
    }

    // ------------------------------------------------------------------------- publish

    fn publish(&self, course_id: &str) -> R<Vec<FollowUp>> {
        let store = &self.deps.store;
        let project = self.project(course_id)?;
        let Some(plan) = store
            .latest_plan(course_id)
            .map_err(store_fail("read plan"))?
            .filter(|p| p.status == PlanStatus::OwnerApproved)
        else {
            return Ok(vec![]);
        };
        let graph = store
            .graph(course_id, plan.concept_graph_revision)
            .map_err(store_fail("read graph"))?
            .ok_or_else(|| fail("the plan's concept graph is gone", false))?;
        let mut latest: BTreeMap<String, LessonDraft> = BTreeMap::new();
        for d in store
            .drafts_for(course_id)
            .map_err(store_fail("read drafts"))?
        {
            if d.plan_revision != plan.revision
                || !matches!(d.stage, LessonStage::Ready | LessonStage::Published)
            {
                continue;
            }
            let newer = latest
                .get(&d.lesson_plan_id)
                .is_none_or(|old| d.created_ms >= old.created_ms);
            if newer {
                latest.insert(d.lesson_plan_id.clone(), d);
            }
        }
        if latest.is_empty() {
            return Ok(vec![]);
        }
        let assembled = assemble_package(&project, &plan, &graph, &latest);
        // Lessons the package couldn't take go back for another draft.
        let mut follow = Vec::new();
        for (lesson_plan_id, why) in &assembled.rejected {
            if let Some(d) = latest.get(lesson_plan_id)
                && d.stage == LessonStage::Ready
            {
                tracing::info!(course = %course_id, lesson = %lesson_plan_id, %why, "course factory: lesson left out of the package");
                let _ = store.advance_stage(&d.draft_id, LessonStage::Ready, LessonStage::Failed);
                follow.push(FollowUp {
                    job: AuthoringJob::GenerateLesson {
                        course_id: course_id.into(),
                        plan_revision: plan.revision,
                        lesson_plan_id: lesson_plan_id.clone(),
                    },
                    reasons: vec![PriorityReason::Pipeline],
                    tie_break: 0,
                });
            }
        }
        let Some(package) = assembled.package else {
            return Ok(follow);
        };
        let mut sources = Vec::new();
        for id in &project.source_ids {
            if let Some(s) = self
                .deps
                .sources
                .get(id)
                .map_err(plain_fail("read source"))?
            {
                sources.push(s);
            }
        }
        let built_from: BTreeSet<SourceAnchorSet> = assembled
            .included
            .iter()
            .filter_map(|id| latest.get(id))
            .flat_map(|d| d.built_from.iter().cloned())
            .collect();
        let receipts: Vec<String> = assembled
            .included
            .iter()
            .filter_map(|id| latest.get(id))
            .flat_map(|d| {
                self.receipts(&d.draft_id)
                    .into_iter()
                    .filter(|r| r.output_hash == d.output_hash)
                    .map(|r| r.receipt_id)
                    .collect::<Vec<_>>()
            })
            .collect();
        let published = self
            .deps
            .packages
            .publish(PublishRequest {
                package: &package,
                receipts,
                built_from: built_from.into_iter().collect(),
                rights: combined_rights(&sources),
                now_ms: self.now(),
            })
            .map_err(plain_fail("publish"))?;
        if let Published::New(_) = published {
            let track = self
                .deps
                .packages
                .latest(course_id)
                .map_err(plain_fail("read published course"))?
                .ok_or_else(|| fail("the published revision is missing", true))?
                .to_track()
                .map_err(plain_fail("load published course"))?;
            self.deps
                .engine
                .publish_generated(track)
                .map_err(|e| fail(format!("the course engine refused it: {e}"), false))?;
        }
        for id in &assembled.included {
            if let Some(d) = latest.get(id)
                && d.stage == LessonStage::Ready
            {
                store
                    .advance_stage(&d.draft_id, LessonStage::Ready, LessonStage::Published)
                    .map_err(store_fail("mark published"))?;
            }
        }
        let planned = plan_lessons(&plan).len();
        if assembled.included.len() == planned {
            self.set_project(course_id, ProjectStatus::Complete)?;
        }
        Ok(follow)
    }
}

impl Pipeline {
    /// The publish step, for an owner action that waits on it.
    pub fn publish_now(&self, course_id: &str) -> Step {
        match self.publish(course_id) {
            Ok(follow_ups) => Step::done(follow_ups),
            Err(Stop(outcome)) => Step::outcome(outcome),
        }
    }
}

/// The objective, its prerequisites (the first objective of each prerequisite concept) and
/// the concept's exercise families.
fn objective_context(
    graph: &ConceptGraph,
    objective_id: &str,
) -> Option<(ObjectiveSpec, Vec<ObjectiveSpec>, Vec<String>)> {
    let concept = graph
        .concepts
        .iter()
        .find(|c| c.objectives.iter().any(|o| o.id == objective_id))?;
    let objective = concept
        .objectives
        .iter()
        .find(|o| o.id == objective_id)?
        .clone();
    let prerequisites = concept
        .prerequisites
        .iter()
        .filter_map(|edge| {
            graph
                .concepts
                .iter()
                .find(|c| c.id == edge.requires)
                .and_then(|c| c.objectives.first().cloned())
        })
        .collect();
    Some((objective, prerequisites, concept.exercise_families.clone()))
}

/// The result of [`assemble_package`].
pub struct Assembled {
    pub package: Option<CoursePackage>,
    /// Lesson plan ids in the package.
    pub included: Vec<String>,
    /// Lesson plan ids left out, with why.
    pub rejected: Vec<(String, String)>,
}

/// Builds the learner package from the latest ready drafts, in plan order. Ids the model
/// chose are namespaced by the server (lesson ids by course, step families and review ids
/// by lesson), so two generated courses never clash and a model can't reuse another
/// lesson's family. A lesson that would make the package invalid is left out and reported,
/// and the rest still publish.
pub fn assemble_package(
    project: &CourseProject,
    plan: &CurriculumPlan,
    graph: &ConceptGraph,
    drafts: &BTreeMap<String, LessonDraft>,
) -> Assembled {
    let first_objective: BTreeMap<&str, &str> = graph
        .concepts
        .iter()
        .filter_map(|c| c.objectives.first().map(|o| (c.id.as_str(), o.id.as_str())))
        .collect();
    let objectives: Vec<Objective> = graph
        .concepts
        .iter()
        .flat_map(|c| {
            let prereqs: Vec<String> = c
                .prerequisites
                .iter()
                .filter_map(|e| {
                    first_objective
                        .get(e.requires.as_str())
                        .map(|o| o.to_string())
                })
                .collect();
            c.objectives.iter().map(move |o| Objective {
                id: o.id.clone(),
                title: o.statement.clone(),
                prerequisites: prereqs.clone(),
            })
        })
        .collect();
    let mut package = CoursePackage {
        id: project.course_id.clone(),
        version: format!("plan-{}", plan.revision),
        title: plan.title.clone(),
        domain: project.domain,
        summary: plan.scope.clone(),
        objectives,
        chapters: Vec::new(),
        lessons: Vec::new(),
        diagnostic: None,
        reviews: Vec::new(),
    };
    let mut included = Vec::new();
    let mut rejected = Vec::new();
    for unit in &plan.units {
        let mut chapter = Chapter {
            id: format!("{}::{}", project.course_id, unit.id),
            title: unit.title.clone(),
            lessons: Vec::new(),
        };
        for lp in &unit.lessons {
            let Some(draft) = drafts.get(&lp.id) else {
                continue;
            };
            let lesson_id = published_lesson_id(&project.course_id, &lp.id);
            let src = &draft.output.lesson;
            let lesson = Lesson {
                id: lesson_id.clone(),
                title: lp.title.clone(),
                minutes: src.minutes,
                objective_ids: src.objective_ids.clone(),
                // Order comes from the chapters; the model's own lesson references don't
                // name published ids, so they're dropped.
                prerequisites: Vec::new(),
                steps: src
                    .steps
                    .iter()
                    .map(|s| namespace_step(s, &lesson_id, None))
                    .collect(),
            };
            let reviews: Vec<CourseStep> = draft
                .output
                .review_variants
                .iter()
                .map(|s| namespace_step(s, &lesson_id, Some("review")))
                .collect();
            let mut candidate = package.clone();
            candidate.lessons.push(lesson.clone());
            candidate.reviews.extend(reviews.clone());
            let mut chapters = candidate.chapters.clone();
            let mut ch = chapter.clone();
            ch.lessons.push(lesson_id.clone());
            chapters.push(ch);
            candidate.chapters = chapters;
            match candidate.validate() {
                Ok(()) => {
                    package.lessons.push(lesson);
                    package.reviews.extend(reviews);
                    chapter.lessons.push(lesson_id);
                    included.push(lp.id.clone());
                }
                Err(why) => rejected.push((lp.id.clone(), why)),
            }
        }
        if !chapter.lessons.is_empty() {
            package.chapters.push(chapter);
        }
    }
    Assembled {
        package: (!package.lessons.is_empty()).then_some(package),
        included,
        rejected,
    }
}

fn namespace_step(step: &CourseStep, lesson_id: &str, role: Option<&str>) -> CourseStep {
    let mut s = step.clone();
    let prefix = match role {
        Some(role) => format!("{lesson_id}::{role}"),
        None => lesson_id.to_string(),
    };
    if role.is_some() {
        s.id = format!("{prefix}::{}", step.id);
    }
    s.family = step.family.as_ref().map(|f| format!("{prefix}::{f}"));
    s
}

#[async_trait::async_trait]
impl JobHandler for Pipeline {
    fn blocked(&self, job: &AuthoringJob) -> Option<String> {
        let course = match job {
            AuthoringJob::ExtractConcepts { course_id, .. }
            | AuthoringJob::BuildConceptGraph { course_id }
            | AuthoringJob::BuildCurriculum { course_id, .. }
            | AuthoringJob::GenerateLesson { course_id, .. }
            | AuthoringJob::GenerateExerciseVariant { course_id, .. }
            | AuthoringJob::GenerateReviewAssessment { course_id, .. }
            | AuthoringJob::PublishCourseRevision { course_id } => course_id,
            _ => return None,
        };
        match self.deps.store.project(course) {
            Ok(Some(p)) if p.status == ProjectStatus::Paused => {
                Some("the owner paused this course".into())
            }
            _ => None,
        }
    }

    async fn run(&self, job: &AuthoringJob, cancel: CancellationToken) -> Step {
        let result = match job {
            AuthoringJob::ExtractSource {
                source_id,
                revision,
            } => self.extract_source(source_id, *revision).await,
            AuthoringJob::ExtractPdfPages {
                source_id,
                revision,
                first_page,
            } => {
                self.extract_pdf_pages(source_id, *revision, *first_page, cancel)
                    .await
            }
            AuthoringJob::ExtractConcepts {
                course_id,
                fragment_ids,
            } => self.extract_concepts(course_id, fragment_ids, cancel).await,
            AuthoringJob::BuildConceptGraph { course_id } => self.build_graph(course_id),
            AuthoringJob::BuildCurriculum {
                course_id,
                concept_graph_revision,
            } => {
                self.build_curriculum(course_id, *concept_graph_revision, cancel)
                    .await
            }
            AuthoringJob::GenerateLesson {
                course_id,
                plan_revision,
                lesson_plan_id,
            } => {
                self.generate_lesson(course_id, *plan_revision, lesson_plan_id, cancel)
                    .await
            }
            AuthoringJob::ValidateDraft { draft_id } => self.validate_draft(draft_id).await,
            AuthoringJob::PublishCourseRevision { course_id } => self.publish(course_id),
            // Review variants come with each lesson draft in this round.
            AuthoringJob::GenerateExerciseVariant { .. }
            | AuthoringJob::GenerateReviewAssessment { .. } => Ok(vec![]),
        };
        match result {
            Ok(follow_ups) => Step::done(follow_ups),
            Err(Stop(outcome)) => Step::outcome(outcome),
        }
    }
}

/// `true` for a source type that may need sandboxed extraction.
pub fn is_pdf(source: &EducationalSource) -> bool {
    matches!(
        source.source_type,
        SourceType::PdfText | SourceType::PdfScanned
    )
}

/// A planned lesson and the newest draft of it, for the course view.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LessonRow {
    pub lesson_plan_id: String,
    pub unit_id: String,
    pub title: String,
    pub stage: Option<LessonStage>,
    pub draft_id: Option<String>,
    pub published_revision: Option<u64>,
}

/// Every planned lesson with its newest draft, in plan order.
pub fn lesson_rows(
    plan: &CurriculumPlan,
    drafts: &[LessonDraft],
    published: Option<u64>,
) -> Vec<LessonRow> {
    let mut newest: BTreeMap<&str, &LessonDraft> = BTreeMap::new();
    for d in drafts.iter().filter(|d| d.plan_revision == plan.revision) {
        let newer = newest
            .get(d.lesson_plan_id.as_str())
            .is_none_or(|old| d.created_ms >= old.created_ms);
        if newer {
            newest.insert(d.lesson_plan_id.as_str(), d);
        }
    }
    plan.units
        .iter()
        .flat_map(|u| {
            u.lessons.iter().map(|l| {
                let d = newest.get(l.id.as_str());
                LessonRow {
                    lesson_plan_id: l.id.clone(),
                    unit_id: u.id.clone(),
                    title: l.title.clone(),
                    stage: d.map(|d| d.stage),
                    draft_id: d.map(|d| d.draft_id.clone()),
                    published_revision: d
                        .filter(|d| d.stage == LessonStage::Published)
                        .and(published),
                }
            })
        })
        .collect()
}

/// Shorthand used by the service: the course id a new project gets from its title.
pub fn course_id_for(title: &str) -> CourseId {
    let slug: String = title
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let short = &uuid::Uuid::new_v4().simple().to_string()[..8];
    if slug.is_empty() {
        format!("gen-{short}")
    } else {
        format!("gen-{slug}-{short}")
    }
}
