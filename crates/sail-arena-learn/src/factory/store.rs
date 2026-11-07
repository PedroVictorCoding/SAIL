//! Durable Course Factory state: owner projects, concept batches, graph and plan revisions,
//! lesson drafts and their pipeline stages.
//!
//! This is the factory's own append-only-ish record store over the shared Arena Learn SQLite
//! file, in `learn_factory_store_*` tables. It never calls a model, never validates content
//! (that is `validate/`) and never assigns provenance: the pipeline hands in server-built
//! records. Its jobs are to remember them exactly, keep revisions monotonic, make stage
//! changes a compare-and-set, and answer "what did this source revision touch?".
//!
//! Model replies are stored as the exact `raw_json` text that produced them, the same way
//! [`super::contracts::LessonDraft`] documents. The parsed proposal or lesson output is
//! recomputed on read, so a stored reply is always re-parseable and can be re-validated
//! without asking the model again. `output_hash` is recomputed on write, so a tampered reply
//! cannot be smuggled in behind a stale hash.
//!
//! Everything is bounded: ids, records, revisions per course, batches, drafts and list
//! pages all have a `pub const` cap. A hostile owner request is refused with a typed error;
//! corrupt stored JSON is reported as a storage error rather than silently skipped.
use super::author::assemble::ConceptBatch;
use super::contracts::{
    ConceptGraph, ConceptProposal, CourseProject, CurriculumPlan, FragmentId, Invalidation,
    LessonDraft, LessonDraftOutput, LessonStage, PlanStatus, ProjectStatus, SourceAnchorSet,
    sha256_hex,
};
use super::registry::PackageRegistry;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;
use std::sync::Mutex;

/// Most projects returned by [`FactoryStore::projects`]. The owner's library is paginated at
/// this size; a bigger library is a product decision, not a query.
pub const MAX_PROJECTS: usize = 1_000;

/// Most sources one course project may name.
pub const MAX_PROJECT_SOURCES: usize = 128;

/// Longest course title accepted when creating a project.
pub const MAX_PROJECT_TITLE_BYTES: usize = 300;

/// Longest level or scope string accepted when creating a project.
pub const MAX_PROJECT_TEXT_BYTES: usize = 4_096;

/// Longest stored identifier (`course_id`, `draft_id`, `lesson_plan_id`, `batch_key`).
pub const MAX_ID_BYTES: usize = 256;

/// Most concept batches one course may keep. A 500-page book at a few dozen fragments per
/// batch stays far below this; the cap keeps a runaway pipeline bounded.
pub const MAX_BATCHES_PER_COURSE: usize = 2_048;

/// Most graph revisions kept per course. Old revisions stay for invalidation and review.
pub const MAX_GRAPH_REVISIONS: u64 = 10_000;

/// Most plan revisions kept per course.
pub const MAX_PLAN_REVISIONS: u64 = 10_000;

/// Most drafts returned for one course. Draft history is kept; the list is a UI page.
pub const MAX_DRAFTS_PER_COURSE: usize = 10_000;

/// Most source-revision anchor sets one record may be built from.
pub const MAX_ANCHOR_SETS: usize = 1_024;

/// Most concept proposals one extraction batch may contain. The prompt caps the batch too;
/// this is the storage-side bound on a model reply.
pub const MAX_CONCEPTS_PER_BATCH: usize = 512;

/// Most concepts one graph may store (the assembler caps at 512; this is the storage bound).
pub const MAX_GRAPH_CONCEPTS: usize = 4_096;

/// Most units one plan may store.
pub const MAX_PLAN_UNITS: usize = 1_024;

/// Largest JSON record stored for one project, graph, plan or draft. A generated lesson is a
/// few tens of KiB; a much larger record is a bug or an attack, not a course.
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Why a factory record could not be stored or read.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The caller handed in an invalid record: empty/oversized id, bad revision order, a
    /// hash that does not match its text, or a record that names an unknown project.
    #[error("invalid factory record: {0}")]
    Invalid(String),
    /// A requested record does not exist.
    #[error("factory record not found: {0}")]
    NotFound(String),
    /// A compare-and-set or revision rule failed. The caller should re-read and decide.
    #[error("factory record conflict: {0}")]
    Conflict(String),
    /// SQLite trouble, or a stored record that no longer parses. Never silently skipped.
    #[error("factory store: {0}")]
    Store(String),
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Store(error.to_string())
}

/// Durable factory state. Owns the connection behind a mutex exactly like
/// [`super::sources::SourceRegistry`], so it is `Send + Sync` and safe to share.
pub struct FactoryStore {
    conn: Mutex<Connection>,
}

impl FactoryStore {
    /// Open or create the store in the Arena Learn database file.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(backend)?;
        }
        Self::init(Connection::open(path).map_err(backend)?)
    }

    /// An in-memory store for tests and ephemeral callers.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory().map_err(backend)?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS learn_factory_store_projects (
                 course_id  TEXT PRIMARY KEY,
                 created_ms INTEGER NOT NULL,
                 status     TEXT NOT NULL,
                 record     TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS learn_factory_store_projects_created
                 ON learn_factory_store_projects(created_ms, course_id);
             CREATE TABLE IF NOT EXISTS learn_factory_store_concept_batches (
                 course_id     TEXT NOT NULL,
                 batch_key     TEXT NOT NULL,
                 raw_json      TEXT NOT NULL,
                 fragment_ids  TEXT NOT NULL,
                 PRIMARY KEY (course_id, batch_key)
             );
             CREATE TABLE IF NOT EXISTS learn_factory_store_graphs (
                 course_id  TEXT NOT NULL,
                 revision   INTEGER NOT NULL,
                 built_from TEXT NOT NULL,
                 record     TEXT NOT NULL,
                 PRIMARY KEY (course_id, revision)
             );
             CREATE TABLE IF NOT EXISTS learn_factory_store_plans (
                 course_id  TEXT NOT NULL,
                 revision   INTEGER NOT NULL,
                 status     TEXT NOT NULL,
                 built_from TEXT NOT NULL,
                 record     TEXT NOT NULL,
                 PRIMARY KEY (course_id, revision)
             );
             CREATE TABLE IF NOT EXISTS learn_factory_store_drafts (
                 draft_id       TEXT PRIMARY KEY,
                 course_id      TEXT NOT NULL,
                 plan_revision  INTEGER NOT NULL,
                 lesson_plan_id TEXT NOT NULL,
                 stage          TEXT NOT NULL,
                 created_ms     INTEGER NOT NULL,
                 output_hash    TEXT NOT NULL,
                 built_from     TEXT NOT NULL,
                 raw_json       TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS learn_factory_store_drafts_course
                 ON learn_factory_store_drafts(course_id, lesson_plan_id, created_ms);
             CREATE INDEX IF NOT EXISTS learn_factory_store_drafts_stage
                 ON learn_factory_store_drafts(course_id, stage);",
        )
        .map_err(backend)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A poisoned lock only means another thread panicked mid-call; SQLite keeps the
        // database consistent, so carry on with the connection.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// --------------------------------------------------------------------------------- projects

impl FactoryStore {
    /// Store a new course project. The id is the project's identity; a duplicate is a
    /// conflict, never an overwrite.
    pub fn create_project(&self, project: CourseProject) -> Result<CourseProject, StoreError> {
        check_project(&project)?;
        let record = serde_json::to_string(&project).map_err(backend)?;
        check_record(&record, "project")?;
        let status = serde_json::to_string(&project.status).map_err(backend)?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO learn_factory_store_projects (course_id, created_ms, status, record)
             VALUES (?1, ?2, ?3, ?4)",
            params![project.course_id, project.created_ms, status, record],
        )
        .map_err(|error| match error {
            rusqlite::Error::SqliteFailure(_, Some(ref m)) if m.contains("UNIQUE") => {
                StoreError::Conflict(format!("project `{}` already exists", project.course_id))
            }
            other => backend(other),
        })?;
        Ok(project)
    }

    /// One project, or `None`.
    pub fn project(&self, course_id: &str) -> Result<Option<CourseProject>, StoreError> {
        let conn = self.lock();
        let record: Option<String> = conn
            .query_row(
                "SELECT record FROM learn_factory_store_projects WHERE course_id = ?1",
                params![course_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        record
            .map(|text| parse_record::<CourseProject>(&text, "project"))
            .transpose()
    }

    /// The owner's projects, oldest first, bounded by [`MAX_PROJECTS`].
    pub fn projects(&self) -> Result<Vec<CourseProject>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT record FROM learn_factory_store_projects
                 ORDER BY created_ms, course_id LIMIT ?1",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map(params![MAX_PROJECTS as i64], |row| row.get::<_, String>(0))
            .map_err(backend)?;
        rows.map(|row| {
            let text = row.map_err(backend)?;
            parse_record::<CourseProject>(&text, "project")
        })
        .collect()
    }

    /// Move a project to a new status and return the stored record. The status is the only
    /// project field the pipeline changes after creation.
    pub fn set_project_status(
        &self,
        course_id: &str,
        status: ProjectStatus,
    ) -> Result<CourseProject, StoreError> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        let record: Option<String> = tx
            .query_row(
                "SELECT record FROM learn_factory_store_projects WHERE course_id = ?1",
                params![course_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(text) = record else {
            return Err(StoreError::NotFound(format!("project `{course_id}`")));
        };
        let mut project = parse_record::<CourseProject>(&text, "project")?;
        project.status = status;
        let record = serde_json::to_string(&project).map_err(backend)?;
        let status_text = serde_json::to_string(&project.status).map_err(backend)?;
        tx.execute(
            "UPDATE learn_factory_store_projects SET status = ?2, record = ?3
             WHERE course_id = ?1",
            params![course_id, status_text, record],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(project)
    }

    fn require_project(conn: &Connection, course_id: &str) -> Result<(), StoreError> {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM learn_factory_store_projects WHERE course_id = ?1",
                params![course_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        if exists.is_some() {
            Ok(())
        } else {
            Err(StoreError::Invalid(format!(
                "unknown course project `{course_id}`"
            )))
        }
    }
}

// --------------------------------------------------------------------------- concept batches

impl FactoryStore {
    /// Save one bounded concept-extraction result. `raw_json` is the model reply exactly as
    /// parsed; on save it must re-parse to `proposal`, so the two can never disagree.
    ///
    /// Saving the same `batch_key` again with the same bytes is an idempotent retry. The same
    /// key with different content is a conflict, because a batch key names one reply.
    pub fn save_concept_batch(
        &self,
        course_id: &str,
        batch_key: &str,
        proposal: &ConceptProposal,
        raw_json: &str,
        fragment_ids_in_prompt: &[FragmentId],
    ) -> Result<(), StoreError> {
        check_id(course_id, "course_id")?;
        check_id(batch_key, "batch_key")?;
        check_record(raw_json, "concept batch")?;
        if proposal.concepts.len() > MAX_CONCEPTS_PER_BATCH {
            return Err(StoreError::Invalid(format!(
                "concept batch has {} concepts, over the {MAX_CONCEPTS_PER_BATCH} cap",
                proposal.concepts.len()
            )));
        }
        for fragment_id in fragment_ids_in_prompt {
            check_id(fragment_id, "fragment_id")?;
        }

        let parsed = serde_json::from_str::<ConceptProposal>(raw_json)
            .map_err(|error| StoreError::Invalid(format!("concept batch raw_json: {error}")))?;
        if &parsed != proposal {
            return Err(StoreError::Invalid(
                "concept batch raw_json does not parse to the supplied proposal".to_owned(),
            ));
        }
        let fragment_ids = serde_json::to_string(fragment_ids_in_prompt).map_err(backend)?;

        let conn = self.lock();
        Self::require_project(&conn, course_id)?;
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO learn_factory_store_concept_batches
                    (course_id, batch_key, raw_json, fragment_ids)
                 VALUES (?1, ?2, ?3, ?4)",
                params![course_id, batch_key, raw_json, fragment_ids],
            )
            .map_err(backend)?;
        if inserted == 1 {
            return Ok(());
        }
        let existing: Option<(String, String)> = conn
            .query_row(
                "SELECT raw_json, fragment_ids FROM learn_factory_store_concept_batches
                 WHERE course_id = ?1 AND batch_key = ?2",
                params![course_id, batch_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(backend)?;
        match existing {
            Some((stored_json, stored_ids))
                if stored_json == raw_json && stored_ids == fragment_ids =>
            {
                Ok(())
            }
            Some(_) => Err(StoreError::Conflict(format!(
                "concept batch `{batch_key}` already exists with different content"
            ))),
            None => Err(StoreError::Store(
                "concept batch insert lost a row without a conflict".to_owned(),
            )),
        }
    }

    /// Every saved batch for a course, in `batch_key` order (so graph assembly is
    /// deterministic across restarts).
    pub fn concept_batches(&self, course_id: &str) -> Result<Vec<ConceptBatch>, StoreError> {
        check_id(course_id, "course_id")?;
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT raw_json, fragment_ids FROM learn_factory_store_concept_batches
                 WHERE course_id = ?1 ORDER BY batch_key LIMIT ?2",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map(params![course_id, MAX_BATCHES_PER_COURSE as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(backend)?;
        let mut batches = Vec::new();
        for row in rows {
            let (raw_json, fragment_ids) = row.map_err(backend)?;
            let proposal = serde_json::from_str::<ConceptProposal>(&raw_json)
                .map_err(|error| backend(format!("corrupt concept batch: {error}")))?;
            let prompt_fragment_ids = serde_json::from_str::<Vec<FragmentId>>(&fragment_ids)
                .map_err(|error| backend(format!("corrupt concept batch fragment ids: {error}")))?;
            batches.push(ConceptBatch {
                prompt_fragment_ids,
                proposal,
            });
        }
        Ok(batches)
    }
}

// ---------------------------------------------------------------------- graph and plan revisions

impl FactoryStore {
    /// Store a new concept graph revision. The first revision is 1 and each later one is
    /// exactly `latest + 1`; revisions are never edited, so an old graph stays referenceable.
    pub fn save_graph(&self, graph: ConceptGraph) -> Result<ConceptGraph, StoreError> {
        check_id(&graph.course_id, "course_id")?;
        if graph.concepts.len() > MAX_GRAPH_CONCEPTS {
            return Err(StoreError::Invalid(format!(
                "graph has {} concepts, over the {MAX_GRAPH_CONCEPTS} cap",
                graph.concepts.len()
            )));
        }
        check_anchor_sets(&graph.built_from)?;
        let record = serde_json::to_string(&graph).map_err(backend)?;
        check_record(&record, "concept graph")?;
        let built_from = serde_json::to_string(&graph.built_from).map_err(backend)?;

        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        Self::require_project(&tx, &graph.course_id)?;
        let expected = next_revision(&tx, "learn_factory_store_graphs", &graph.course_id)?;
        if graph.revision != expected {
            return Err(StoreError::Conflict(format!(
                "graph revision {} is not the expected {expected}",
                graph.revision
            )));
        }
        if graph.revision > MAX_GRAPH_REVISIONS {
            return Err(StoreError::Invalid(format!(
                "graph revision {} exceeds the {MAX_GRAPH_REVISIONS} cap",
                graph.revision
            )));
        }
        tx.execute(
            "INSERT INTO learn_factory_store_graphs (course_id, revision, built_from, record)
             VALUES (?1, ?2, ?3, ?4)",
            params![graph.course_id, graph.revision as i64, built_from, record],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(graph)
    }

    /// One stored graph revision, or `None`.
    pub fn graph(
        &self,
        course_id: &str,
        revision: u64,
    ) -> Result<Option<ConceptGraph>, StoreError> {
        let conn = self.lock();
        let record: Option<String> = conn
            .query_row(
                "SELECT record FROM learn_factory_store_graphs
                 WHERE course_id = ?1 AND revision = ?2",
                params![course_id, revision as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        record
            .map(|text| parse_record::<ConceptGraph>(&text, "concept graph"))
            .transpose()
    }

    /// The newest graph revision for a course, or `None`.
    pub fn latest_graph(&self, course_id: &str) -> Result<Option<ConceptGraph>, StoreError> {
        let conn = self.lock();
        let record: Option<String> = conn
            .query_row(
                "SELECT record FROM learn_factory_store_graphs
                 WHERE course_id = ?1 ORDER BY revision DESC LIMIT 1",
                params![course_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        record
            .map(|text| parse_record::<ConceptGraph>(&text, "concept graph"))
            .transpose()
    }

    /// Store a proposed plan revision. It must reference an existing graph revision; the
    /// graph's `built_from` is copied in so later invalidation does not have to re-walk the
    /// plan. Revisions follow the same "latest + 1" rule as graphs.
    pub fn save_plan(&self, plan: CurriculumPlan) -> Result<CurriculumPlan, StoreError> {
        check_id(&plan.course_id, "course_id")?;
        check_text(&plan.title, "plan title", MAX_PROJECT_TEXT_BYTES)?;
        if plan.units.len() > MAX_PLAN_UNITS {
            return Err(StoreError::Invalid(format!(
                "plan has {} units, over the {MAX_PLAN_UNITS} cap",
                plan.units.len()
            )));
        }
        if plan.status != PlanStatus::Proposed {
            return Err(StoreError::Invalid(
                "a saved plan must be Proposed; use approve_plan to approve one".to_owned(),
            ));
        }
        let record = serde_json::to_string(&plan).map_err(backend)?;
        check_record(&record, "curriculum plan")?;

        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        Self::require_project(&tx, &plan.course_id)?;
        let expected = next_revision(&tx, "learn_factory_store_plans", &plan.course_id)?;
        if plan.revision != expected {
            return Err(StoreError::Conflict(format!(
                "plan revision {} is not the expected {expected}",
                plan.revision
            )));
        }
        if plan.revision > MAX_PLAN_REVISIONS {
            return Err(StoreError::Invalid(format!(
                "plan revision {} exceeds the {MAX_PLAN_REVISIONS} cap",
                plan.revision
            )));
        }
        let built_from: Option<String> = tx
            .query_row(
                "SELECT built_from FROM learn_factory_store_graphs
                 WHERE course_id = ?1 AND revision = ?2",
                params![plan.course_id, plan.concept_graph_revision as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(built_from) = built_from else {
            return Err(StoreError::Invalid(format!(
                "plan names unknown graph revision {}",
                plan.concept_graph_revision
            )));
        };
        let status = plan_status_text(plan.status)?;
        tx.execute(
            "INSERT INTO learn_factory_store_plans
                (course_id, revision, status, built_from, record)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                plan.course_id,
                plan.revision as i64,
                status,
                built_from,
                record
            ],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(plan)
    }

    /// One stored plan revision, or `None`.
    pub fn plan(
        &self,
        course_id: &str,
        revision: u64,
    ) -> Result<Option<CurriculumPlan>, StoreError> {
        let conn = self.lock();
        let record: Option<String> = conn
            .query_row(
                "SELECT record FROM learn_factory_store_plans
                 WHERE course_id = ?1 AND revision = ?2",
                params![course_id, revision as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        record
            .map(|text| parse_record::<CurriculumPlan>(&text, "curriculum plan"))
            .transpose()
    }

    /// The newest plan revision for a course, or `None`.
    pub fn latest_plan(&self, course_id: &str) -> Result<Option<CurriculumPlan>, StoreError> {
        let conn = self.lock();
        let record: Option<String> = conn
            .query_row(
                "SELECT record FROM learn_factory_store_plans
                 WHERE course_id = ?1 ORDER BY revision DESC LIMIT 1",
                params![course_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        record
            .map(|text| parse_record::<CurriculumPlan>(&text, "curriculum plan"))
            .transpose()
    }

    /// Approve one plan revision. Every older plan becomes `Superseded`; the approved
    /// revision keeps its content and is never re-saved. Re-approving is a no-op.
    pub fn approve_plan(
        &self,
        course_id: &str,
        revision: u64,
    ) -> Result<CurriculumPlan, StoreError> {
        check_id(course_id, "course_id")?;
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        let record: Option<String> = tx
            .query_row(
                "SELECT record FROM learn_factory_store_plans
                 WHERE course_id = ?1 AND revision = ?2",
                params![course_id, revision as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(text) = record else {
            return Err(StoreError::NotFound(format!(
                "plan `{course_id}` revision {revision}"
            )));
        };
        let mut plan = parse_record::<CurriculumPlan>(&text, "curriculum plan")?;
        match plan.status {
            PlanStatus::OwnerApproved => return Ok(plan),
            PlanStatus::Superseded => {
                return Err(StoreError::Conflict(format!(
                    "plan `{course_id}` revision {revision} is superseded"
                )));
            }
            PlanStatus::Proposed => {}
        }

        let older: Vec<(i64, String)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT revision, record FROM learn_factory_store_plans
                     WHERE course_id = ?1 AND revision < ?2 ORDER BY revision",
                )
                .map_err(backend)?;
            let rows = stmt
                .query_map(params![course_id, revision as i64], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(backend)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(backend)?
        };
        for (older_revision, older_text) in older {
            let mut older_plan = parse_record::<CurriculumPlan>(&older_text, "curriculum plan")?;
            if older_plan.status != PlanStatus::Superseded {
                older_plan.status = PlanStatus::Superseded;
                let older_record = serde_json::to_string(&older_plan).map_err(backend)?;
                tx.execute(
                    "UPDATE learn_factory_store_plans SET status = ?3, record = ?4
                     WHERE course_id = ?1 AND revision = ?2",
                    params![
                        course_id,
                        older_revision,
                        plan_status_text(PlanStatus::Superseded)?,
                        older_record
                    ],
                )
                .map_err(backend)?;
            }
        }

        plan.status = PlanStatus::OwnerApproved;
        let record = serde_json::to_string(&plan).map_err(backend)?;
        tx.execute(
            "UPDATE learn_factory_store_plans SET status = ?3, record = ?4
             WHERE course_id = ?1 AND revision = ?2",
            params![
                course_id,
                revision as i64,
                plan_status_text(plan.status)?,
                record
            ],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(plan)
    }
}

// ---------------------------------------------------------------------------------- drafts

impl FactoryStore {
    /// Store one generated draft. `output_hash` must equal the SHA-256 of `raw_json`; the
    /// reply must parse as a [`LessonDraftOutput`]. A duplicate draft id is a conflict.
    pub fn save_draft(&self, draft: &LessonDraft) -> Result<(), StoreError> {
        check_id(&draft.draft_id, "draft_id")?;
        check_id(&draft.course_id, "course_id")?;
        check_id(&draft.lesson_plan_id, "lesson_plan_id")?;
        check_record(&draft.raw_json, "lesson draft")?;
        check_anchor_sets(&draft.built_from)?;
        let computed = sha256_hex(draft.raw_json.as_bytes());
        if computed != draft.output_hash {
            return Err(StoreError::Invalid(format!(
                "draft `{}` output_hash does not match its raw_json",
                draft.draft_id
            )));
        }
        serde_json::from_str::<LessonDraftOutput>(&draft.raw_json).map_err(|error| {
            StoreError::Invalid(format!(
                "draft `{}` raw_json does not parse: {error}",
                draft.draft_id
            ))
        })?;
        let built_from = serde_json::to_string(&draft.built_from).map_err(backend)?;
        let stage = stage_text(draft.stage)?;
        let conn = self.lock();
        Self::require_project(&conn, &draft.course_id)?;
        conn.execute(
            "INSERT INTO learn_factory_store_drafts
                (draft_id, course_id, plan_revision, lesson_plan_id, stage, created_ms,
                 output_hash, built_from, raw_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                draft.draft_id,
                draft.course_id,
                draft.plan_revision as i64,
                draft.lesson_plan_id,
                stage,
                draft.created_ms,
                draft.output_hash,
                built_from,
                draft.raw_json
            ],
        )
        .map_err(|error| match error {
            rusqlite::Error::SqliteFailure(_, Some(ref m)) if m.contains("UNIQUE") => {
                StoreError::Conflict(format!("draft `{}` already exists", draft.draft_id))
            }
            other => backend(other),
        })?;
        Ok(())
    }

    /// One stored draft, or `None`.
    pub fn draft(&self, draft_id: &str) -> Result<Option<LessonDraft>, StoreError> {
        let conn = self.lock();
        let raw: Option<RawDraft> = conn
            .query_row(
                "SELECT draft_id, course_id, plan_revision, lesson_plan_id, stage, created_ms,
                        output_hash, built_from, raw_json
                 FROM learn_factory_store_drafts WHERE draft_id = ?1",
                params![draft_id],
                RawDraft::read,
            )
            .optional()
            .map_err(backend)?;
        raw.map(RawDraft::decode).transpose()
    }

    /// Every draft for a course, oldest first, bounded by [`MAX_DRAFTS_PER_COURSE`].
    pub fn drafts_for(&self, course_id: &str) -> Result<Vec<LessonDraft>, StoreError> {
        check_id(course_id, "course_id")?;
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT draft_id, course_id, plan_revision, lesson_plan_id, stage, created_ms,
                        output_hash, built_from, raw_json
                 FROM learn_factory_store_drafts
                 WHERE course_id = ?1 ORDER BY created_ms, draft_id LIMIT ?2",
            )
            .map_err(backend)?;
        collect_drafts(&mut stmt, params![course_id, MAX_DRAFTS_PER_COURSE as i64])
    }

    /// The newest draft for one planned lesson, or `None`.
    pub fn latest_draft_for(
        &self,
        course_id: &str,
        lesson_plan_id: &str,
    ) -> Result<Option<LessonDraft>, StoreError> {
        check_id(course_id, "course_id")?;
        check_id(lesson_plan_id, "lesson_plan_id")?;
        let conn = self.lock();
        let raw: Option<RawDraft> = conn
            .query_row(
                "SELECT draft_id, course_id, plan_revision, lesson_plan_id, stage, created_ms,
                        output_hash, built_from, raw_json
                 FROM learn_factory_store_drafts
                 WHERE course_id = ?1 AND lesson_plan_id = ?2
                 ORDER BY created_ms DESC, draft_id DESC LIMIT 1",
                params![course_id, lesson_plan_id],
                RawDraft::read,
            )
            .optional()
            .map_err(backend)?;
        raw.map(RawDraft::decode).transpose()
    }

    /// Advance a draft's stage as a compare-and-set: the stored stage must be exactly
    /// `from`, the move must be forward, and a terminal stage only leaves through
    /// `NeedsReview -> Ready` (owner approval). Returns the new stage.
    pub fn advance_stage(
        &self,
        draft_id: &str,
        from: LessonStage,
        to: LessonStage,
    ) -> Result<LessonStage, StoreError> {
        check_id(draft_id, "draft_id")?;
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        let current: Option<String> = tx
            .query_row(
                "SELECT stage FROM learn_factory_store_drafts WHERE draft_id = ?1",
                params![draft_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(current) = current else {
            return Err(StoreError::NotFound(format!("draft `{draft_id}`")));
        };
        let current = stage_from_text(&current)?;
        if current != from {
            return Err(StoreError::Conflict(format!(
                "draft `{draft_id}` is at `{current:?}`, not `{from:?}`"
            )));
        }
        if !transition_allowed(from, to) {
            return Err(StoreError::Conflict(format!(
                "draft `{draft_id}` cannot move from `{from:?}` to `{to:?}`"
            )));
        }
        tx.execute(
            "UPDATE learn_factory_store_drafts SET stage = ?2 WHERE draft_id = ?1",
            params![draft_id, stage_text(to)?],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(to)
    }
}

/// Whether one draft-stage move is legal. Forward means later in [`LessonStage`]'s declared
/// order; terminal stages only leave through the owner-approval exception.
fn transition_allowed(from: LessonStage, to: LessonStage) -> bool {
    if from == to {
        return false;
    }
    if from.is_terminal() {
        return from == LessonStage::NeedsReview && to == LessonStage::Ready;
    }
    to > from
}

struct RawDraft {
    draft_id: String,
    course_id: String,
    plan_revision: i64,
    lesson_plan_id: String,
    stage: String,
    created_ms: i64,
    output_hash: String,
    built_from: String,
    raw_json: String,
}

impl RawDraft {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            draft_id: row.get(0)?,
            course_id: row.get(1)?,
            plan_revision: row.get(2)?,
            lesson_plan_id: row.get(3)?,
            stage: row.get(4)?,
            created_ms: row.get(5)?,
            output_hash: row.get(6)?,
            built_from: row.get(7)?,
            raw_json: row.get(8)?,
        })
    }

    fn decode(self) -> Result<LessonDraft, StoreError> {
        let output = serde_json::from_str::<LessonDraftOutput>(&self.raw_json)
            .map_err(|error| backend(format!("corrupt draft `{}`: {error}", self.draft_id)))?;
        let built_from =
            serde_json::from_str::<Vec<SourceAnchorSet>>(&self.built_from).map_err(|error| {
                backend(format!(
                    "corrupt draft `{}` provenance: {error}",
                    self.draft_id
                ))
            })?;
        Ok(LessonDraft {
            draft_id: self.draft_id,
            course_id: self.course_id,
            plan_revision: self.plan_revision as u64,
            lesson_plan_id: self.lesson_plan_id,
            output,
            raw_json: self.raw_json,
            output_hash: self.output_hash,
            built_from,
            stage: stage_from_text(&self.stage)?,
            created_ms: self.created_ms,
        })
    }
}

fn collect_drafts(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<LessonDraft>, StoreError> {
    let rows = stmt.query_map(params, RawDraft::read).map_err(backend)?;
    let mut drafts = Vec::new();
    for row in rows {
        drafts.push(row.map_err(backend)?.decode()?);
    }
    Ok(drafts)
}

// ---------------------------------------------------------------------------- invalidation

impl FactoryStore {
    /// Find everything built from the given source revisions and mark drafts invalid.
    ///
    /// `affected` are the `SourceAnchorSet`s reported by
    /// [`super::sources::SourceRegistry::affected_by`]. Graphs and plans are reported for
    /// re-validation; drafts move to [`LessonStage::SourceInvalidated`]. When a package
    /// registry is supplied, published revisions whose provenance matches are reported too
    /// (the latest revision of each course, which is what the engine serves).
    pub fn invalidate(
        &self,
        affected: &[SourceAnchorSet],
        packages: Option<&PackageRegistry>,
    ) -> Result<Invalidation, StoreError> {
        if affected.is_empty() {
            return Ok(Invalidation::default());
        }
        let affected_json = serde_json::to_string(affected).map_err(backend)?;

        let mut invalidation = Invalidation::default();
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;

        for (course_id, revision) in
            matching_revisions(&tx, "learn_factory_store_graphs", &affected_json)?
        {
            invalidation.concept_graphs.push((course_id, revision));
        }
        for (course_id, revision) in
            matching_revisions(&tx, "learn_factory_store_plans", &affected_json)?
        {
            invalidation.plans.push((course_id, revision));
        }

        let draft_ids: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "SELECT d.draft_id FROM learn_factory_store_drafts d
                     WHERE EXISTS (
                         SELECT 1 FROM json_each(d.built_from) AS b, json_each(?1) AS a
                         WHERE json_extract(b.value, '$.source_id') = json_extract(a.value, '$.source_id')
                           AND json_extract(b.value, '$.source_revision') = json_extract(a.value, '$.source_revision')
                           AND json_extract(b.value, '$.content_hash') = json_extract(a.value, '$.content_hash')
                     )
                     ORDER BY d.draft_id",
                )
                .map_err(backend)?;
            let rows = stmt
                .query_map(params![affected_json], |row| row.get::<_, String>(0))
                .map_err(backend)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(backend)?
        };
        for draft_id in &draft_ids {
            tx.execute(
                "UPDATE learn_factory_store_drafts SET stage = ?2 WHERE draft_id = ?1",
                params![draft_id, stage_text(LessonStage::SourceInvalidated)?],
            )
            .map_err(backend)?;
        }
        invalidation.drafts = draft_ids;
        tx.commit().map_err(backend)?;
        drop(conn);

        if let Some(registry) = packages {
            let latest = registry
                .latest_all()
                .map_err(|error| backend(error.to_string()))?;
            for revision in latest {
                if revision
                    .built_from
                    .iter()
                    .any(|anchor| affected.contains(anchor))
                {
                    invalidation
                        .revisions
                        .push((revision.course_id, revision.revision));
                }
            }
        }

        invalidation.concept_graphs.sort();
        invalidation.plans.sort();
        invalidation.revisions.sort();
        Ok(invalidation)
    }
}

fn matching_revisions(
    conn: &Connection,
    table: &str,
    affected_json: &str,
) -> Result<Vec<(String, u64)>, StoreError> {
    // `table` is a compile-time constant chosen by the caller; no user text reaches SQL.
    let sql = format!(
        "SELECT r.course_id, r.revision FROM {table} r
         WHERE EXISTS (
             SELECT 1 FROM json_each(r.built_from) AS b, json_each(?1) AS a
             WHERE json_extract(b.value, '$.source_id') = json_extract(a.value, '$.source_id')
               AND json_extract(b.value, '$.source_revision') = json_extract(a.value, '$.source_revision')
               AND json_extract(b.value, '$.content_hash') = json_extract(a.value, '$.content_hash')
         )
         ORDER BY r.course_id, r.revision"
    );
    let mut stmt = conn.prepare(&sql).map_err(backend)?;
    let rows = stmt
        .query_map(params![affected_json], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })
        .map_err(backend)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(backend)
}

// ------------------------------------------------------------------------------- validation

fn next_revision(conn: &Connection, table: &str, course_id: &str) -> Result<u64, StoreError> {
    // `table` is a compile-time constant chosen by the caller; no user text reaches SQL.
    let sql = format!("SELECT MAX(revision) FROM {table} WHERE course_id = ?1");
    let latest: Option<i64> = conn
        .query_row(&sql, params![course_id], |row| row.get(0))
        .map_err(backend)?;
    Ok(latest.map(|revision| revision as u64 + 1).unwrap_or(1))
}

fn check_project(project: &CourseProject) -> Result<(), StoreError> {
    check_id(&project.course_id, "course_id")?;
    check_text(&project.title, "project title", MAX_PROJECT_TITLE_BYTES)?;
    check_text(&project.level, "project level", MAX_PROJECT_TEXT_BYTES)?;
    if project.source_ids.is_empty() {
        return Err(StoreError::Invalid(
            "a course project needs at least one source".to_owned(),
        ));
    }
    if project.source_ids.len() > MAX_PROJECT_SOURCES {
        return Err(StoreError::Invalid(format!(
            "project names {} sources, over the {MAX_PROJECT_SOURCES} cap",
            project.source_ids.len()
        )));
    }
    for source_id in &project.source_ids {
        check_id(source_id, "source_id")?;
    }
    Ok(())
}

fn check_anchor_sets(anchors: &[SourceAnchorSet]) -> Result<(), StoreError> {
    if anchors.len() > MAX_ANCHOR_SETS {
        return Err(StoreError::Invalid(format!(
            "record is built from {} source revisions, over the {MAX_ANCHOR_SETS} cap",
            anchors.len()
        )));
    }
    for anchor in anchors {
        check_id(&anchor.source_id, "source_id")?;
        if anchor.source_revision == 0 {
            return Err(StoreError::Invalid(
                "source_revision must be at least 1".to_owned(),
            ));
        }
        check_id(&anchor.content_hash, "content_hash")?;
    }
    Ok(())
}

fn check_id(value: &str, field: &str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(StoreError::Invalid(format!("{field} is empty")));
    }
    if value.len() > MAX_ID_BYTES {
        return Err(StoreError::Invalid(format!(
            "{field} is {} bytes, over the {MAX_ID_BYTES} cap",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(StoreError::Invalid(format!(
            "{field} contains a control character"
        )));
    }
    Ok(())
}

fn check_text(value: &str, field: &str, max_bytes: usize) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(StoreError::Invalid(format!("{field} is empty")));
    }
    if value.len() > max_bytes {
        return Err(StoreError::Invalid(format!(
            "{field} is {} bytes, over the {max_bytes} cap",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(StoreError::Invalid(format!(
            "{field} contains a control character"
        )));
    }
    Ok(())
}

fn check_record(text: &str, what: &str) -> Result<(), StoreError> {
    if text.len() > MAX_RECORD_BYTES {
        return Err(StoreError::Invalid(format!(
            "{what} is {} bytes, over the {MAX_RECORD_BYTES} cap",
            text.len()
        )));
    }
    Ok(())
}

fn parse_record<T: serde::de::DeserializeOwned>(text: &str, what: &str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(|error| backend(format!("corrupt {what}: {error}")))
}

fn stage_text(stage: LessonStage) -> Result<String, StoreError> {
    match serde_json::to_value(stage).map_err(backend)?.as_str() {
        Some(text) => Ok(text.to_owned()),
        None => Err(StoreError::Store(
            "lesson stage did not serialize as a string".to_owned(),
        )),
    }
}

fn stage_from_text(text: &str) -> Result<LessonStage, StoreError> {
    serde_json::from_value(serde_json::Value::String(text.to_owned()))
        .map_err(|error| backend(format!("corrupt draft stage `{text}`: {error}")))
}

fn plan_status_text(status: PlanStatus) -> Result<String, StoreError> {
    match serde_json::to_value(status).map_err(backend)?.as_str() {
        Some(text) => Ok(text.to_owned()),
        None => Err(StoreError::Store(
            "plan status did not serialize as a string".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::courses::content::Domain;
    use crate::factory::contracts::{
        LessonPlan, LessonPlanKind, ProposedConcept, UnitPlan, ValidatorKind,
    };
    use std::collections::BTreeMap;

    const NOW_MS: i64 = 1_794_000_000_000;

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("sail-factory-store-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("temp directory");
            Self(path)
        }

        fn file(&self) -> std::path::PathBuf {
            self.0.join("factory.sqlite")
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn project(course_id: &str) -> CourseProject {
        CourseProject {
            course_id: course_id.to_owned(),
            title: "Systems".to_owned(),
            domain: Domain::Rust,
            source_ids: vec!["src-rust".to_owned()],
            level: "intro".to_owned(),
            status: ProjectStatus::Extracting,
            created_ms: NOW_MS,
        }
    }

    fn anchor(source_id: &str, revision: u64, hash: &str) -> SourceAnchorSet {
        SourceAnchorSet {
            source_id: source_id.to_owned(),
            source_revision: revision,
            content_hash: hash.to_owned(),
        }
    }

    fn graph(course_id: &str, revision: u64, anchor: SourceAnchorSet) -> ConceptGraph {
        ConceptGraph {
            course_id: course_id.to_owned(),
            revision,
            built_from: vec![anchor],
            concepts: Vec::new(),
        }
    }

    fn plan(course_id: &str, revision: u64, graph_revision: u64) -> CurriculumPlan {
        CurriculumPlan {
            course_id: course_id.to_owned(),
            revision,
            concept_graph_revision: graph_revision,
            title: "Systems".to_owned(),
            domain: Domain::Rust,
            level: "intro".to_owned(),
            scope: "ownership".to_owned(),
            outcomes: vec!["Explain moves.".to_owned()],
            units: vec![UnitPlan {
                id: "unit.1".to_owned(),
                title: "Ownership".to_owned(),
                objective_ids: vec!["rust.ownership.objective.1".to_owned()],
                lessons: vec![LessonPlan {
                    id: "lesson.1".to_owned(),
                    title: "Moves".to_owned(),
                    objective_ids: vec!["rust.ownership.objective.1".to_owned()],
                    minutes: 5,
                    kind: LessonPlanKind::Lesson,
                }],
                review_checkpoint: false,
            }],
            conflicts: Vec::new(),
            status: PlanStatus::Proposed,
        }
    }

    const DRAFT_JSON: &str = r#"{"lesson":{"id":"lesson.1","title":"Moves","minutes":5,"objective_ids":["o.1"],"steps":[]},"claims":[]}"#;

    fn draft(
        id: &str,
        course_id: &str,
        lesson_plan_id: &str,
        anchor: SourceAnchorSet,
    ) -> LessonDraft {
        let output = serde_json::from_str::<LessonDraftOutput>(DRAFT_JSON).expect("draft output");
        LessonDraft {
            draft_id: id.to_owned(),
            course_id: course_id.to_owned(),
            plan_revision: 1,
            lesson_plan_id: lesson_plan_id.to_owned(),
            output,
            raw_json: DRAFT_JSON.to_owned(),
            output_hash: sha256_hex(DRAFT_JSON.as_bytes()),
            built_from: vec![anchor],
            stage: LessonStage::DraftGenerated,
            created_ms: NOW_MS,
        }
    }

    fn proposal() -> ConceptProposal {
        ConceptProposal {
            concepts: vec![ProposedConcept {
                id: "rust.ownership".to_owned(),
                name: "Ownership".to_owned(),
                description: "One owner at a time.".to_owned(),
                objectives: vec!["The learner can explain moves.".to_owned()],
                prerequisites: BTreeMap::new(),
                fragment_ids: vec!["f-1".to_owned()],
                difficulty: 2,
                validator: ValidatorKind::ExactKey,
            }],
        }
    }

    const PROPOSAL_JSON: &str = r#"{"concepts":[{"id":"rust.ownership","name":"Ownership","description":"One owner at a time.","objectives":["The learner can explain moves."],"prerequisites":{},"fragment_ids":["f-1"],"difficulty":2,"validator":"exact_key"}]}"#;

    #[test]
    fn projects_round_trip_and_status_changes() {
        let store = FactoryStore::open_in_memory().expect("store");
        let created = store
            .create_project(project("course.rust"))
            .expect("create");
        assert_eq!(created, project("course.rust"));
        assert_eq!(store.project("course.rust").expect("get"), Some(created));
        assert!(store.project("course.missing").expect("get").is_none());

        let updated = store
            .set_project_status("course.rust", ProjectStatus::Generating)
            .expect("status");
        assert_eq!(updated.status, ProjectStatus::Generating);
        assert_eq!(store.projects().expect("list"), vec![updated]);
        assert!(matches!(
            store.set_project_status("course.missing", ProjectStatus::Paused),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.create_project(project("course.rust")),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn restart_keeps_projects_graphs_plans_and_drafts() {
        let directory = Directory::new();
        {
            let store = FactoryStore::open(&directory.file()).expect("open");
            store
                .create_project(project("course.rust"))
                .expect("project");
            store
                .save_graph(graph("course.rust", 1, anchor("src-rust", 1, "hash-a")))
                .expect("graph");
            store.save_plan(plan("course.rust", 1, 1)).expect("plan");
            store
                .save_draft(&draft(
                    "draft.1",
                    "course.rust",
                    "lesson.1",
                    anchor("src-rust", 1, "hash-a"),
                ))
                .expect("draft");
        }
        let store = FactoryStore::open(&directory.file()).expect("reopen");
        assert!(store.project("course.rust").expect("project").is_some());
        assert_eq!(
            store
                .latest_graph("course.rust")
                .expect("graph")
                .map(|g| g.revision),
            Some(1)
        );
        assert_eq!(
            store
                .latest_plan("course.rust")
                .expect("plan")
                .map(|p| p.revision),
            Some(1)
        );
        assert_eq!(
            store.draft("draft.1").expect("draft").map(|d| d.stage),
            Some(LessonStage::DraftGenerated)
        );
    }

    #[test]
    fn concept_batches_are_ordered_and_idempotent() {
        let store = FactoryStore::open_in_memory().expect("store");
        store
            .create_project(project("course.rust"))
            .expect("project");
        store
            .save_concept_batch(
                "course.rust",
                "batch-b",
                &proposal(),
                PROPOSAL_JSON,
                &["f-1".to_owned()],
            )
            .expect("save b");
        store
            .save_concept_batch(
                "course.rust",
                "batch-a",
                &proposal(),
                PROPOSAL_JSON,
                &["f-1".to_owned()],
            )
            .expect("save a");
        store
            .save_concept_batch(
                "course.rust",
                "batch-a",
                &proposal(),
                PROPOSAL_JSON,
                &["f-1".to_owned()],
            )
            .expect("idempotent retry");

        let batches = store.concept_batches("course.rust").expect("batches");
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].prompt_fragment_ids, vec!["f-1".to_owned()]);
        assert_eq!(batches[0].proposal, proposal());

        let other = ConceptProposal {
            concepts: Vec::new(),
        };
        assert!(matches!(
            store.save_concept_batch("course.rust", "batch-a", &other, r#"{"concepts":[]}"#, &[]),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn graph_revisions_follow_latest_plus_one() {
        let store = FactoryStore::open_in_memory().expect("store");
        store
            .create_project(project("course.rust"))
            .expect("project");
        assert!(matches!(
            store.save_graph(graph("course.rust", 2, anchor("src-rust", 1, "hash-a"))),
            Err(StoreError::Conflict(_))
        ));
        store
            .save_graph(graph("course.rust", 1, anchor("src-rust", 1, "hash-a")))
            .expect("rev 1");
        assert!(matches!(
            store.save_graph(graph("course.rust", 3, anchor("src-rust", 1, "hash-a"))),
            Err(StoreError::Conflict(_))
        ));
        store
            .save_graph(graph("course.rust", 2, anchor("src-rust", 1, "hash-a")))
            .expect("rev 2");
        assert_eq!(
            store
                .latest_graph("course.rust")
                .expect("latest")
                .map(|g| g.revision),
            Some(2)
        );
        assert_eq!(
            store
                .graph("course.rust", 1)
                .expect("rev 1")
                .map(|g| g.revision),
            Some(1)
        );
        assert!(matches!(
            store.save_graph(graph("course.missing", 1, anchor("src-rust", 1, "hash-a"))),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn plans_copy_graph_provenance_and_approval_supersedes_older() {
        let store = FactoryStore::open_in_memory().expect("store");
        store
            .create_project(project("course.rust"))
            .expect("project");
        assert!(matches!(
            store.save_plan(plan("course.rust", 1, 1)),
            Err(StoreError::Invalid(_))
        ));
        store
            .save_graph(graph("course.rust", 1, anchor("src-rust", 1, "old-hash")))
            .expect("graph 1");
        store.save_plan(plan("course.rust", 1, 1)).expect("plan 1");
        store
            .save_graph(graph("course.rust", 2, anchor("src-rust", 2, "new-hash")))
            .expect("graph 2");
        store.save_plan(plan("course.rust", 2, 2)).expect("plan 2");

        let approved = store.approve_plan("course.rust", 2).expect("approve");
        assert_eq!(approved.status, PlanStatus::OwnerApproved);
        assert_eq!(
            store
                .plan("course.rust", 1)
                .expect("plan 1")
                .map(|p| p.status),
            Some(PlanStatus::Superseded)
        );
        assert_eq!(
            store
                .latest_plan("course.rust")
                .expect("latest")
                .map(|p| p.status),
            Some(PlanStatus::OwnerApproved)
        );
        store.approve_plan("course.rust", 2).expect("idempotent");
        assert!(matches!(
            store.approve_plan("course.rust", 1),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.save_plan(plan("course.rust", 2, 2)),
            Err(StoreError::Conflict(_))
        ));
        let mut approved_again = plan("course.rust", 3, 2);
        approved_again.status = PlanStatus::OwnerApproved;
        assert!(matches!(
            store.save_plan(approved_again),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn draft_hash_is_recomputed_and_bad_json_is_refused() {
        let store = FactoryStore::open_in_memory().expect("store");
        store
            .create_project(project("course.rust"))
            .expect("project");
        let good = draft(
            "draft.1",
            "course.rust",
            "lesson.1",
            anchor("src-rust", 1, "hash-a"),
        );
        store.save_draft(&good).expect("draft");
        assert!(matches!(
            store.save_draft(&good),
            Err(StoreError::Conflict(_))
        ));

        let mut tampered = good.clone();
        tampered.draft_id = "draft.2".to_owned();
        tampered.output_hash = "not-the-hash".to_owned();
        assert!(matches!(
            store.save_draft(&tampered),
            Err(StoreError::Invalid(_))
        ));

        let mut unparseable = good.clone();
        unparseable.draft_id = "draft.3".to_owned();
        unparseable.raw_json = "{\"lesson\":{}}".to_owned();
        unparseable.output_hash = sha256_hex(unparseable.raw_json.as_bytes());
        assert!(matches!(
            store.save_draft(&unparseable),
            Err(StoreError::Invalid(_))
        ));

        assert_eq!(
            store
                .latest_draft_for("course.rust", "lesson.1")
                .expect("latest")
                .map(|d| d.draft_id),
            Some("draft.1".to_owned())
        );
        assert_eq!(store.drafts_for("course.rust").expect("drafts").len(), 1);
    }

    #[test]
    fn draft_stage_is_compare_and_set() {
        let store = FactoryStore::open_in_memory().expect("store");
        store
            .create_project(project("course.rust"))
            .expect("project");
        let mut queued = draft(
            "draft.1",
            "course.rust",
            "lesson.1",
            anchor("src-rust", 1, "hash-a"),
        );
        queued.stage = LessonStage::Queued;
        store.save_draft(&queued).expect("draft");

        assert_eq!(
            store
                .advance_stage("draft.1", LessonStage::Queued, LessonStage::DraftGenerated)
                .expect("forward"),
            LessonStage::DraftGenerated
        );
        assert!(matches!(
            store.advance_stage("draft.1", LessonStage::Queued, LessonStage::DraftGenerated),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.advance_stage("draft.1", LessonStage::DraftGenerated, LessonStage::Queued),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.advance_stage(
                "draft.missing",
                LessonStage::Queued,
                LessonStage::DraftGenerated
            ),
            Err(StoreError::NotFound(_))
        ));
        store
            .advance_stage("draft.1", LessonStage::DraftGenerated, LessonStage::Ready)
            .expect("skip ahead is forward");
        store
            .advance_stage("draft.1", LessonStage::Ready, LessonStage::Published)
            .expect("publish");
        assert!(matches!(
            store.advance_stage("draft.1", LessonStage::Published, LessonStage::NeedsReview),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn needs_review_is_the_only_terminal_stage_that_can_be_approved() {
        let store = FactoryStore::open_in_memory().expect("store");
        store
            .create_project(project("course.rust"))
            .expect("project");
        for (index, stage) in [
            LessonStage::Failed,
            LessonStage::Cancelled,
            LessonStage::Blocked,
            LessonStage::SourceInvalidated,
            LessonStage::Published,
        ]
        .into_iter()
        .enumerate()
        {
            let mut stored = draft(
                &format!("draft.{index}"),
                "course.rust",
                "lesson.1",
                anchor("src-rust", 1, "hash-a"),
            );
            stored.stage = stage;
            store.save_draft(&stored).expect("draft");
            assert!(
                store
                    .advance_stage(&stored.draft_id, stage, LessonStage::Ready)
                    .is_err(),
                "`{stage:?}` must not leave to Ready"
            );
        }

        let mut review = draft(
            "draft.review",
            "course.rust",
            "lesson.1",
            anchor("src-rust", 1, "hash-a"),
        );
        review.stage = LessonStage::NeedsReview;
        store.save_draft(&review).expect("draft");
        assert_eq!(
            store
                .advance_stage("draft.review", LessonStage::NeedsReview, LessonStage::Ready)
                .expect("owner approval"),
            LessonStage::Ready
        );
    }

    #[test]
    fn invalidation_marks_only_matching_records() {
        let store = FactoryStore::open_in_memory().expect("store");
        store
            .create_project(project("course.rust"))
            .expect("project");
        store
            .create_project(project("course.other"))
            .expect("project");

        let rust = anchor("src-rust", 1, "hash-rust");
        let other = anchor("src-other", 1, "hash-other");
        store
            .save_graph(graph("course.rust", 1, rust.clone()))
            .expect("graph");
        store
            .save_graph(graph("course.other", 1, other.clone()))
            .expect("graph");
        store.save_plan(plan("course.rust", 1, 1)).expect("plan");
        store
            .save_draft(&draft(
                "draft.rust",
                "course.rust",
                "lesson.1",
                rust.clone(),
            ))
            .expect("draft");
        store
            .save_draft(&draft(
                "draft.other",
                "course.other",
                "lesson.1",
                other.clone(),
            ))
            .expect("draft");

        let invalidation = store.invalidate(&[rust], None).expect("invalidate");
        assert_eq!(
            invalidation.concept_graphs,
            vec![("course.rust".to_owned(), 1)]
        );
        assert_eq!(invalidation.plans, vec![("course.rust".to_owned(), 1)]);
        assert_eq!(invalidation.drafts, vec!["draft.rust".to_owned()]);
        assert!(invalidation.revisions.is_empty());
        assert_eq!(
            store.draft("draft.rust").expect("draft").map(|d| d.stage),
            Some(LessonStage::SourceInvalidated)
        );
        assert_eq!(
            store.draft("draft.other").expect("draft").map(|d| d.stage),
            Some(LessonStage::DraftGenerated)
        );
        assert!(
            store
                .invalidate(&[anchor("src-none", 1, "none")], None)
                .expect("no match")
                .concept_graphs
                .is_empty()
        );
    }

    #[test]
    fn invalid_inputs_are_refused_before_storage() {
        let store = FactoryStore::open_in_memory().expect("store");
        let mut bad = project("course.rust");
        bad.title = String::new();
        assert!(matches!(
            store.create_project(bad),
            Err(StoreError::Invalid(_))
        ));

        let mut no_sources = project("course.rust");
        no_sources.source_ids.clear();
        assert!(matches!(
            store.create_project(no_sources),
            Err(StoreError::Invalid(_))
        ));

        let oversized = project("course.rust");
        store.create_project(oversized).expect("project");
        let record = "x".repeat(MAX_RECORD_BYTES + 1);
        assert!(matches!(
            store.save_concept_batch("course.rust", "batch", &proposal(), &record, &[]),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.save_concept_batch("course.rust", "", &proposal(), PROPOSAL_JSON, &[]),
            Err(StoreError::Invalid(_))
        ));

        let mut bad_anchor = anchor("src-rust", 1, "hash-a");
        bad_anchor.source_revision = 0;
        assert!(matches!(
            store.save_graph(graph("course.rust", 1, bad_anchor)),
            Err(StoreError::Invalid(_))
        ));
    }
}
