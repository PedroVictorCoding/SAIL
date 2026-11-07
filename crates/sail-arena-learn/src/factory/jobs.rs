//! The Course Factory's background queue: small, durable [`AuthoringJob`]s stored as
//! Continuity tasks, picked in a deterministic priority order and run one at a time.
//!
//! Why Continuity: a job survives a daemon restart (recovery marks a job that was running
//! as interrupted, and it is picked again), at most one job per dedupe key is active, and
//! the task history is the audit trail the inspector shows.
//!
//! The queue never runs work itself. A [`Worker`] asks it for the next admissible job,
//! hands the job to a [`JobHandler`] (the pipeline), and reports the [`JobOutcome`]. A job
//! that can't run now (the model is busy with the owner, the sandbox is down, the day's
//! budget is spent) is **deferred**, not failed: it goes back on the queue with a
//! not-before time and keeps its failure count.
use super::contracts::{AuthorModelError, AuthoringJob, PriorityReason, QueuedJob};
use super::settings::SettingsStore;
use sail_continuity::{
    CancelOutcome, Continuity, ContinuityError, Lease, Outcome, ResumePolicy, TaskFilter, TaskSpec,
    TaskState,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Continuity owner of every factory task.
pub const OWNER: &str = "sail-arena-learn";
/// Prefix of the Continuity `kind`, followed by the job's own kind (`learn.factory.generate_lesson`).
pub const KIND_PREFIX: &str = "learn.factory.";
/// Failed attempts before a job stops retrying and waits for the owner.
pub const MAX_FAILURES: u32 = 5;
/// First retry delay after a failure; doubles per failure up to [`MAX_BACKOFF_MS`].
pub const BACKOFF_BASE_MS: i64 = 60_000;
pub const MAX_BACKOFF_MS: i64 = 6 * 60 * 60_000;
/// Most active jobs the queue reads at once. A course of 300 lessons stays well below it;
/// a runaway producer is stopped by [`JobQueue::enqueue`] instead of growing the scan.
pub const MAX_ACTIVE_JOBS: usize = 5_000;
/// Finished jobs shown in the inspector's "recent" list.
pub const MAX_RECENT: usize = 50;
/// Longest error text stored with a job. Model and parser errors can quote input; keep them
/// short so a stored error can't carry a page of a book.
pub const MAX_ERROR_CHARS: usize = 300;

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("job queue storage: {0}")]
    Store(String),
    #[error("the queue is full ({MAX_ACTIVE_JOBS} active jobs)")]
    Full,
    #[error("a job with the same key needs the owner first: {0}")]
    NeedsOwner(String),
}

impl From<ContinuityError> for JobError {
    fn from(e: ContinuityError) -> Self {
        match e {
            ContinuityError::DuplicateNeedsIntervention { task, .. } => Self::NeedsOwner(task),
            other => Self::Store(other.to_string()),
        }
    }
}

/// What is stored as the Continuity checkpoint. Only the server writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Checkpoint {
    queued: QueuedJob,
    /// Failed attempts so far (deferrals don't count).
    failures: u32,
    /// The job isn't picked before this time (backoff, waiting for the model).
    not_before_ms: i64,
    last_error: Option<String>,
}

/// An active (queued, interrupted or running) job, as the inspector sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pending {
    pub task_id: String,
    pub queued: QueuedJob,
    pub state: TaskState,
    pub not_before_ms: i64,
    pub failures: u32,
    pub last_error: Option<String>,
}

/// A finished job for the "recent" list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recent {
    pub task_id: String,
    pub job: AuthoringJob,
    pub state: TaskState,
    pub error: Option<String>,
    pub at_ms: i64,
}

/// Result of [`JobQueue::enqueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enqueued {
    New(String),
    /// The same work was already queued; its priority was raised if this request ranks higher.
    Existing(String),
}

impl Enqueued {
    pub fn task_id(&self) -> &str {
        match self {
            Self::New(id) | Self::Existing(id) => id,
        }
    }
}

/// A job a worker has claimed. Pass it back to [`JobQueue::finish`].
#[derive(Debug)]
pub struct Claimed {
    pub task_id: String,
    pub queued: QueuedJob,
    lease: Lease,
    failures: u32,
}

/// How a job attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    Done,
    /// Couldn't run now and isn't broken: try again after `retry_after_ms`. No failure counted.
    Defer {
        reason: String,
        retry_after_ms: i64,
    },
    /// The attempt failed. Retryable failures back off and retry up to [`MAX_FAILURES`].
    Failed {
        reason: String,
        retryable: bool,
    },
}

impl JobOutcome {
    /// How the scheduler reads the model's refusals: waiting is not failing.
    pub fn from_model_error(e: &AuthorModelError) -> Self {
        match e {
            AuthorModelError::Unavailable(r) => Self::Defer {
                reason: format!("model unavailable: {r}"),
                retry_after_ms: 5 * 60_000,
            },
            AuthorModelError::Preempted(r) => Self::Defer {
                reason: format!("yielded to foreground work: {r}"),
                retry_after_ms: 30_000,
            },
            AuthorModelError::NotAdmitted(r) => Self::Defer {
                reason: format!("not admitted: {r}"),
                retry_after_ms: 2 * 60_000,
            },
            AuthorModelError::Failed(r) => Self::Failed {
                reason: format!("model call failed: {r}"),
                retryable: true,
            },
        }
    }
}

fn kind_of(job: &AuthoringJob) -> String {
    let tag = serde_json::to_value(job)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "job".into());
    format!("{KIND_PREFIX}{tag}")
}

fn title_of(job: &AuthoringJob) -> String {
    match job {
        AuthoringJob::ExtractSource { source_id, .. } => format!("Read source {source_id}"),
        AuthoringJob::ExtractPdfPages {
            source_id,
            first_page,
            ..
        } => format!("Read {source_id} from page {first_page}"),
        AuthoringJob::ExtractConcepts { course_id, .. } => format!("Find concepts for {course_id}"),
        AuthoringJob::BuildConceptGraph { course_id } => format!("Map concepts of {course_id}"),
        AuthoringJob::BuildCurriculum { course_id, .. } => format!("Plan {course_id}"),
        AuthoringJob::GenerateLesson { lesson_plan_id, .. } => {
            format!("Write lesson {lesson_plan_id}")
        }
        AuthoringJob::GenerateExerciseVariant {
            objective_id,
            family,
            ..
        } => format!("Write a {family} exercise for {objective_id}"),
        AuthoringJob::GenerateReviewAssessment { objective_id, .. } => {
            format!("Write a review for {objective_id}")
        }
        AuthoringJob::ValidateDraft { draft_id } => format!("Check draft {draft_id}"),
        AuthoringJob::PublishCourseRevision { course_id } => format!("Publish {course_id}"),
    }
}

/// Deterministic priority: the best (lowest) tier among the reasons, then the caller's
/// tie-break. No reasons means plumbing ([`PriorityReason::Pipeline`]).
pub fn priority(reasons: &[PriorityReason], tie_break: i64) -> (u8, i64) {
    let tier = reasons
        .iter()
        .map(PriorityReason::tier)
        .min()
        .unwrap_or_else(|| PriorityReason::Pipeline.tier());
    (tier, tie_break)
}

fn clip(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_ERROR_CHARS).collect();
    if text.chars().count() > MAX_ERROR_CHARS {
        out.push('…');
    }
    out
}

fn backoff_ms(failures: u32) -> i64 {
    let shift = failures.saturating_sub(1).min(20);
    BACKOFF_BASE_MS
        .saturating_mul(1_i64 << shift)
        .min(MAX_BACKOFF_MS)
}

/// The factory's view of Continuity.
pub struct JobQueue {
    tasks: Arc<Continuity>,
}

impl JobQueue {
    pub fn new(tasks: Arc<Continuity>) -> Self {
        Self { tasks }
    }

    fn submit(&self, cp: &Checkpoint) -> Result<sail_continuity::Submitted, JobError> {
        let checkpoint = serde_json::to_value(cp).map_err(|e| JobError::Store(e.to_string()))?;
        Ok(self.tasks.submit(
            TaskSpec::new(OWNER, kind_of(&cp.queued.job), title_of(&cp.queued.job))
                .with_dedupe_key(cp.queued.job.dedupe_key())
                // Every job is local and idempotent: re-running one after a crash is safe.
                .with_resume(ResumePolicy::Restart)
                .with_checkpoint(checkpoint),
        )?)
    }

    /// Queue `job`. The same work already queued is not duplicated; if this request ranks
    /// higher (the owner pressed "next lesson now"), the queued job is re-queued at the
    /// better priority with the reasons merged. A running job is left alone.
    pub fn enqueue(
        &self,
        job: AuthoringJob,
        reasons: Vec<PriorityReason>,
        tie_break: i64,
        now_ms: i64,
    ) -> Result<Enqueued, JobError> {
        let wanted = priority(&reasons, tie_break);
        let key = job.dedupe_key();
        let active = self.active()?;
        if let Some(existing) = active.iter().find(|p| p.queued.job.dedupe_key() == key) {
            if wanted >= existing.queued.priority || existing.state == TaskState::Running {
                return Ok(Enqueued::Existing(existing.task_id.clone()));
            }
            // Cancel-and-resubmit is the only way to change a queued task's checkpoint
            // without a lease. If it started meanwhile, keep the running one.
            if self
                .tasks
                .request_cancel(&existing.task_id, "re-queued at a higher priority")?
                != CancelOutcome::Cancelled
            {
                return Ok(Enqueued::Existing(existing.task_id.clone()));
            }
            let mut merged = existing.queued.reasons.clone();
            for r in reasons {
                if !merged.contains(&r) {
                    merged.push(r);
                }
            }
            let cp = Checkpoint {
                queued: QueuedJob {
                    job,
                    priority: priority(&merged, wanted.1),
                    reasons: merged,
                },
                failures: existing.failures,
                // The owner asked for it now; don't make them wait out a backoff.
                not_before_ms: now_ms.min(existing.not_before_ms),
                last_error: existing.last_error.clone(),
            };
            return Ok(Enqueued::New(self.submit(&cp)?.task.id));
        }
        if active.len() >= MAX_ACTIVE_JOBS {
            return Err(JobError::Full);
        }
        let cp = Checkpoint {
            queued: QueuedJob {
                job,
                reasons,
                priority: wanted,
            },
            failures: 0,
            not_before_ms: now_ms,
            last_error: None,
        };
        let submitted = self.submit(&cp)?;
        Ok(if submitted.created {
            Enqueued::New(submitted.task.id)
        } else {
            Enqueued::Existing(submitted.task.id)
        })
    }

    /// Every active factory job, best priority first. A task whose checkpoint doesn't parse
    /// (written by an incompatible build) is failed so it can't block its dedupe key.
    pub fn active(&self) -> Result<Vec<Pending>, JobError> {
        let tasks = self.tasks.list(&TaskFilter {
            owner: Some(OWNER.into()),
            include_terminal: false,
            limit: Some(MAX_ACTIVE_JOBS + 1),
        })?;
        let mut out = Vec::with_capacity(tasks.len());
        for task in tasks {
            if task.state == TaskState::NeedsIntervention {
                continue;
            }
            match serde_json::from_value::<Checkpoint>(task.checkpoint.clone()) {
                Ok(cp) => out.push(Pending {
                    task_id: task.id,
                    queued: cp.queued,
                    state: task.state,
                    not_before_ms: cp.not_before_ms,
                    failures: cp.failures,
                    last_error: cp.last_error,
                }),
                Err(e) if task.state != TaskState::Running => {
                    let _ = self
                        .tasks
                        .fail_unclaimed(&task.id, &clip(&format!("unreadable job: {e}")));
                }
                Err(_) => {}
            }
        }
        out.sort_by(|a, b| {
            (a.queued.priority, a.not_before_ms, &a.task_id).cmp(&(
                b.queued.priority,
                b.not_before_ms,
                &b.task_id,
            ))
        });
        Ok(out)
    }

    /// Claim the best job that is due and that `admit` accepts. `admit` returns why a job
    /// can't run now (model jobs while the owner is busy, a paused course); such jobs are
    /// skipped, not deferred, so they run as soon as the reason clears.
    pub fn next(
        &self,
        now_ms: i64,
        admit: impl Fn(&AuthoringJob) -> Result<(), String>,
    ) -> Result<Option<Claimed>, JobError> {
        for pending in self.active()? {
            if !matches!(pending.state, TaskState::Queued | TaskState::Interrupted)
                || pending.not_before_ms > now_ms
                || admit(&pending.queued.job).is_err()
            {
                continue;
            }
            match self.tasks.claim(&pending.task_id) {
                Ok(lease) => {
                    return Ok(Some(Claimed {
                        task_id: pending.task_id,
                        queued: pending.queued,
                        lease,
                        failures: pending.failures,
                    }));
                }
                // Cancelled or claimed between the scan and now: look at the next one.
                Err(ContinuityError::Cancelled(_)) | Err(ContinuityError::InvalidState { .. }) => {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    }

    /// Whether the owner cancelled the claimed job while it ran.
    pub fn cancel_requested(&self, claimed: &Claimed) -> bool {
        self.tasks
            .is_cancel_requested(&claimed.task_id)
            .unwrap_or(false)
    }

    /// Record the outcome. A deferral or a retryable failure ends this task and queues a
    /// fresh one under the same dedupe key, carrying the failure count and not-before time.
    pub fn finish(
        &self,
        claimed: Claimed,
        outcome: JobOutcome,
        now_ms: i64,
    ) -> Result<(), JobError> {
        let requeue = |failures: u32, not_before_ms: i64, error: String| Checkpoint {
            queued: claimed.queued.clone(),
            failures,
            not_before_ms,
            last_error: Some(error),
        };
        match outcome {
            JobOutcome::Done => {
                self.tasks.finish(&claimed.lease, Outcome::Succeeded)?;
            }
            JobOutcome::Defer {
                reason,
                retry_after_ms,
            } => {
                let reason = clip(&reason);
                self.tasks.finish(
                    &claimed.lease,
                    Outcome::Cancelled(format!("deferred: {reason}")),
                )?;
                let wait = retry_after_ms.clamp(1_000, MAX_BACKOFF_MS);
                self.submit(&requeue(claimed.failures, now_ms + wait, reason))?;
            }
            JobOutcome::Failed { reason, retryable } => {
                let reason = clip(&reason);
                let failures = claimed.failures + 1;
                self.tasks
                    .finish(&claimed.lease, Outcome::Failed(reason.clone()))?;
                if retryable && failures < MAX_FAILURES {
                    self.submit(&requeue(failures, now_ms + backoff_ms(failures), reason))?;
                }
            }
        }
        Ok(())
    }

    /// Cancel every queued job `matches` selects (e.g. a removed source's extraction).
    /// Running jobs get a cancel request their worker sees. Returns how many were touched.
    pub fn cancel_where(
        &self,
        matches: impl Fn(&AuthoringJob) -> bool,
        reason: &str,
    ) -> Result<usize, JobError> {
        let mut n = 0;
        for p in self.active()? {
            if matches(&p.queued.job) {
                self.tasks.request_cancel(&p.task_id, reason)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// The last finished jobs, newest first. Deferrals are bookkeeping, not history, so
    /// they're left out.
    pub fn recent(&self, limit: usize) -> Result<Vec<Recent>, JobError> {
        let limit = limit.min(MAX_RECENT);
        let tasks = self.tasks.list(&TaskFilter {
            owner: Some(OWNER.into()),
            include_terminal: true,
            limit: Some(limit * 4 + 16),
        })?;
        Ok(tasks
            .into_iter()
            .filter(|t| t.state.is_terminal())
            .filter(|t| {
                !(t.state == TaskState::Cancelled
                    && t.reason
                        .as_deref()
                        .is_some_and(|r| r.starts_with("deferred:") || r.starts_with("re-queued")))
            })
            .filter_map(|t| {
                let cp: Checkpoint = serde_json::from_value(t.checkpoint).ok()?;
                Some(Recent {
                    task_id: t.id,
                    job: cp.queued.job,
                    state: t.state,
                    error: t.reason,
                    at_ms: t.updated_at.timestamp_millis(),
                })
            })
            .take(limit)
            .collect())
    }
}

// ---------------------------------------------------------------------------------- worker

/// More work found by a job: the pipeline's way of chaining stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUp {
    pub job: AuthoringJob,
    pub reasons: Vec<PriorityReason>,
    pub tie_break: i64,
}

/// What one handler call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub outcome: JobOutcome,
    pub follow_ups: Vec<FollowUp>,
}

impl Step {
    pub fn done(follow_ups: Vec<FollowUp>) -> Self {
        Self {
            outcome: JobOutcome::Done,
            follow_ups,
        }
    }

    pub fn outcome(outcome: JobOutcome) -> Self {
        Self {
            outcome,
            follow_ups: Vec::new(),
        }
    }
}

/// Runs one job. The pipeline implements it; tests use a scripted fake.
#[async_trait::async_trait]
pub trait JobHandler: Send + Sync {
    /// Why `job` must not start now (its course is paused, its source was removed), if so.
    fn blocked(&self, _job: &AuthoringJob) -> Option<String> {
        None
    }

    async fn run(&self, job: &AuthoringJob, cancel: CancellationToken) -> Step;
}

/// Whether background model work may start. Implemented by the Mind wiring.
pub trait ModelGate: Send + Sync {
    /// `Err(reason)` while foreground model work (Dialogue, voice, an open lesson) runs or
    /// memory is tight. Checked before a model job is claimed.
    fn admit_background(&self) -> Result<(), String>;
}

/// What one [`Worker::tick`] did. Lets the loop pick its sleep and tests assert behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// Off, paused, or nothing is due.
    Idle(String),
    Ran {
        task_id: String,
        job: AuthoringJob,
        outcome: JobOutcome,
        follow_ups: usize,
    },
}

/// Clock source; injected so tests control time.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Picks and runs jobs one at a time under the owner's settings and the daily budget.
pub struct Worker {
    queue: Arc<JobQueue>,
    settings: Arc<SettingsStore>,
    handler: Arc<dyn JobHandler>,
    gate: Arc<dyn ModelGate>,
    clock: Clock,
}

impl Worker {
    pub fn new(
        queue: Arc<JobQueue>,
        settings: Arc<SettingsStore>,
        handler: Arc<dyn JobHandler>,
        gate: Arc<dyn ModelGate>,
        clock: Clock,
    ) -> Self {
        Self {
            queue,
            settings,
            handler,
            gate,
            clock,
        }
    }

    /// Run at most one job. Never panics on a storage error: it reports `Idle` with the
    /// reason, and the loop tries again later.
    pub async fn tick(&self, cancel: &CancellationToken) -> Tick {
        let now = (self.clock)();
        let settings = match self.settings.settings() {
            Ok(s) => s,
            Err(e) => return Tick::Idle(format!("settings unavailable: {e}")),
        };
        if !settings.enabled {
            return Tick::Idle("the factory is off".into());
        }
        if self.settings.paused().unwrap_or(false) {
            return Tick::Idle("paused by the owner".into());
        }
        let budget_left = self.settings.remaining_ms(&settings, now).unwrap_or(0) > 0;
        let model_ok = if !budget_left {
            Err("today's model budget is spent".to_string())
        } else if settings.idle_only {
            self.gate.admit_background()
        } else {
            Ok(())
        };
        let handler = self.handler.clone();
        let admit = |job: &AuthoringJob| {
            if let Some(why) = handler.blocked(job) {
                return Err(why);
            }
            if job.uses_model() {
                model_ok.clone()
            } else {
                Ok(())
            }
        };
        let claimed = match self.queue.next(now, admit) {
            Ok(Some(c)) => c,
            Ok(None) => {
                return Tick::Idle(match &model_ok {
                    Err(why) => format!("nothing to do without the model ({why})"),
                    Ok(()) => "nothing is due".into(),
                });
            }
            Err(e) => return Tick::Idle(format!("queue unavailable: {e}")),
        };
        let job = claimed.queued.job.clone();
        let task_id = claimed.task_id.clone();
        let child = cancel.child_token();
        let started = (self.clock)();
        let step = self.handler.run(&job, child).await;
        let ended = (self.clock)();
        if job.uses_model() {
            let _ = self.settings.record_model_ms(ended, ended - started);
        }
        let outcome = if self.queue.cancel_requested(&claimed) {
            JobOutcome::Failed {
                reason: "cancelled by the owner".into(),
                retryable: false,
            }
        } else if cancel.is_cancelled() && step.outcome != JobOutcome::Done {
            // The daemon is shutting down: this isn't the job's fault.
            JobOutcome::Defer {
                reason: "the daemon stopped".into(),
                retry_after_ms: 1_000,
            }
        } else {
            step.outcome
        };
        let mut queued = 0;
        if outcome == JobOutcome::Done {
            // Follow-ups are queued before the job is marked done: a crash in between
            // re-runs this job, and dedupe keys keep the follow-ups single.
            for f in step.follow_ups {
                if self
                    .queue
                    .enqueue(f.job, f.reasons, f.tie_break, ended)
                    .is_ok()
                {
                    queued += 1;
                }
            }
        }
        if let Err(e) = self.queue.finish(claimed, outcome.clone(), ended) {
            tracing::warn!(error = %e, task = %task_id, "course factory: couldn't record a job outcome");
        }
        Tick::Ran {
            task_id,
            job,
            outcome,
            follow_ups: queued,
        }
    }

    /// The background loop. Runs jobs back to back while there is work, and sleeps
    /// `idle_sleep` between checks otherwise. Stops when `cancel` fires.
    pub async fn run(&self, cancel: CancellationToken, idle_sleep: std::time::Duration) {
        while !cancel.is_cancelled() {
            match self.tick(&cancel).await {
                Tick::Ran { .. } => tokio::task::yield_now().await,
                Tick::Idle(_) => {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(idle_sleep) => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::settings::FactorySettings;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn queue() -> Arc<JobQueue> {
        Arc::new(JobQueue::new(Arc::new(
            Continuity::open_in_memory().unwrap(),
        )))
    }

    fn lesson(id: &str) -> AuthoringJob {
        AuthoringJob::GenerateLesson {
            course_id: "c".into(),
            plan_revision: 1,
            lesson_plan_id: id.into(),
        }
    }

    fn validate(id: &str) -> AuthoringJob {
        AuthoringJob::ValidateDraft {
            draft_id: id.into(),
        }
    }

    fn ok(_: &AuthoringJob) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn order_is_tier_then_tie_break_and_deterministic() {
        let q = queue();
        q.enqueue(lesson("late"), vec![PriorityReason::Enrichment], 0, 0)
            .unwrap();
        q.enqueue(
            lesson("weak"),
            vec![PriorityReason::WeakSkill {
                objective_id: "o".into(),
            }],
            5,
            0,
        )
        .unwrap();
        q.enqueue(
            lesson("weak-first"),
            vec![PriorityReason::WeakSkill {
                objective_id: "o".into(),
            }],
            1,
            0,
        )
        .unwrap();
        q.enqueue(validate("d"), vec![], 0, 0).unwrap();
        let order: Vec<String> = q
            .active()
            .unwrap()
            .into_iter()
            .map(|p| p.queued.job.dedupe_key())
            .collect();
        assert_eq!(
            order,
            [
                "validate:d",
                "lesson:c:weak-first",
                "lesson:c:weak",
                "lesson:c:late"
            ]
        );
    }

    #[test]
    fn same_work_is_not_duplicated_but_an_owner_request_raises_it() {
        let q = queue();
        let first = q
            .enqueue(lesson("l1"), vec![PriorityReason::Enrichment], 0, 0)
            .unwrap();
        let again = q
            .enqueue(lesson("l1"), vec![PriorityReason::Enrichment], 0, 0)
            .unwrap();
        assert_eq!(again, Enqueued::Existing(first.task_id().into()));
        let raised = q
            .enqueue(lesson("l1"), vec![PriorityReason::OwnerRequest], 0, 0)
            .unwrap();
        assert!(matches!(raised, Enqueued::New(_)));
        let active = q.active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].queued.priority.0, 0);
        assert_eq!(active[0].queued.reasons.len(), 2);
    }

    #[test]
    fn deferral_keeps_failures_and_waits() {
        let q = queue();
        q.enqueue(lesson("l1"), vec![], 0, 0).unwrap();
        let c = q.next(0, ok).unwrap().unwrap();
        q.finish(
            c,
            JobOutcome::Defer {
                reason: "busy".into(),
                retry_after_ms: 60_000,
            },
            1_000,
        )
        .unwrap();
        assert!(q.next(30_000, ok).unwrap().is_none());
        let c = q.next(61_000, ok).unwrap().unwrap();
        assert_eq!(c.failures, 0);
        assert!(q.recent(10).unwrap().is_empty(), "deferrals aren't history");
    }

    #[test]
    fn retryable_failures_back_off_then_stop() {
        let q = queue();
        q.enqueue(lesson("l1"), vec![], 0, 0).unwrap();
        let mut now = 0;
        for n in 1..=MAX_FAILURES {
            let c = q.next(now, ok).unwrap().expect("job due");
            q.finish(
                c,
                JobOutcome::Failed {
                    reason: "bad json".into(),
                    retryable: true,
                },
                now,
            )
            .unwrap();
            if n < MAX_FAILURES {
                assert!(q.next(now + backoff_ms(n) - 1, ok).unwrap().is_none());
            }
            now += backoff_ms(n);
        }
        assert!(q.active().unwrap().is_empty());
        assert_eq!(q.recent(10).unwrap().len(), MAX_FAILURES as usize);
    }

    #[test]
    fn admission_skips_without_losing_the_job() {
        let q = queue();
        q.enqueue(lesson("l1"), vec![PriorityReason::OwnerRequest], 0, 0)
            .unwrap();
        q.enqueue(validate("d"), vec![PriorityReason::Enrichment], 0, 0)
            .unwrap();
        let no_model = |j: &AuthoringJob| {
            if j.uses_model() {
                Err("busy".to_string())
            } else {
                Ok(())
            }
        };
        let c = q.next(0, no_model).unwrap().unwrap();
        assert_eq!(c.queued.job, validate("d"));
        q.finish(c, JobOutcome::Done, 0).unwrap();
        assert!(q.next(0, no_model).unwrap().is_none());
        assert_eq!(q.next(0, ok).unwrap().unwrap().queued.job, lesson("l1"));
    }

    #[test]
    fn a_crash_mid_job_resumes_it_after_restart() {
        let dir = std::env::temp_dir().join(format!("sail-cf-jobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("continuity.db");
        {
            let q = JobQueue::new(Arc::new(Continuity::open(&path).unwrap()));
            q.enqueue(lesson("l1"), vec![], 0, 0).unwrap();
            let _claimed = q.next(0, ok).unwrap().unwrap();
            // Dropped without finishing: the daemon died mid-job.
        }
        let tasks = Arc::new(Continuity::open(&path).unwrap());
        tasks.recover().unwrap();
        let q = JobQueue::new(tasks);
        let c = q
            .next(0, ok)
            .unwrap()
            .expect("interrupted job is picked again");
        assert_eq!(c.queued.job, lesson("l1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn errors_are_clipped() {
        let q = queue();
        q.enqueue(lesson("l1"), vec![], 0, 0).unwrap();
        let c = q.next(0, ok).unwrap().unwrap();
        q.finish(
            c,
            JobOutcome::Failed {
                reason: "x".repeat(10_000),
                retryable: false,
            },
            0,
        )
        .unwrap();
        let r = &q.recent(1).unwrap()[0];
        assert!(r.error.as_ref().unwrap().chars().count() <= MAX_ERROR_CHARS + 1);
    }

    #[test]
    fn model_errors_map_to_wait_or_fail() {
        assert!(matches!(
            JobOutcome::from_model_error(&AuthorModelError::Preempted("voice".into())),
            JobOutcome::Defer { .. }
        ));
        assert!(matches!(
            JobOutcome::from_model_error(&AuthorModelError::Unavailable("down".into())),
            JobOutcome::Defer { .. }
        ));
        assert!(matches!(
            JobOutcome::from_model_error(&AuthorModelError::Failed("x".into())),
            JobOutcome::Failed {
                retryable: true,
                ..
            }
        ));
    }

    // ----------------------------------------------------------------- worker

    struct Script {
        steps: Mutex<Vec<Step>>,
        seen: Mutex<Vec<AuthoringJob>>,
        advance: Arc<AtomicI64>,
    }

    #[async_trait::async_trait]
    impl JobHandler for Script {
        fn blocked(&self, job: &AuthoringJob) -> Option<String> {
            matches!(job, AuthoringJob::PublishCourseRevision { .. }).then(|| "paused".into())
        }

        async fn run(&self, job: &AuthoringJob, _cancel: CancellationToken) -> Step {
            self.seen.lock().unwrap().push(job.clone());
            self.advance.fetch_add(90_000, Ordering::SeqCst);
            let mut steps = self.steps.lock().unwrap();
            if steps.is_empty() {
                Step::done(vec![])
            } else {
                steps.remove(0)
            }
        }
    }

    struct Gate(Mutex<Result<(), String>>);
    impl ModelGate for Gate {
        fn admit_background(&self) -> Result<(), String> {
            self.0.lock().unwrap().clone()
        }
    }

    fn worker(
        steps: Vec<Step>,
    ) -> (
        Worker,
        Arc<JobQueue>,
        Arc<SettingsStore>,
        Arc<Script>,
        Arc<Gate>,
    ) {
        let q = queue();
        let settings = Arc::new(SettingsStore::open_in_memory().unwrap());
        let time = Arc::new(AtomicI64::new(1_000));
        let script = Arc::new(Script {
            steps: Mutex::new(steps),
            seen: Mutex::new(vec![]),
            advance: time.clone(),
        });
        let gate = Arc::new(Gate(Mutex::new(Ok(()))));
        let clock: Clock = Arc::new(move || time.load(Ordering::SeqCst));
        let w = Worker::new(
            q.clone(),
            settings.clone(),
            script.clone(),
            gate.clone(),
            clock,
        );
        (w, q, settings, script, gate)
    }

    #[tokio::test]
    async fn follow_ups_chain_stages() {
        let (w, q, _, script, _) = worker(vec![Step::done(vec![FollowUp {
            job: validate("d1"),
            reasons: vec![],
            tie_break: 0,
        }])]);
        q.enqueue(lesson("l1"), vec![], 0, 0).unwrap();
        let cancel = CancellationToken::new();
        assert!(matches!(
            w.tick(&cancel).await,
            Tick::Ran { follow_ups: 1, .. }
        ));
        assert!(matches!(w.tick(&cancel).await, Tick::Ran { .. }));
        assert!(matches!(w.tick(&cancel).await, Tick::Idle(_)));
        assert_eq!(
            *script.seen.lock().unwrap(),
            vec![lesson("l1"), validate("d1")]
        );
    }

    #[tokio::test]
    async fn off_paused_busy_and_budget_all_stop_model_work() {
        let (w, q, settings, script, gate) = worker(vec![]);
        q.enqueue(lesson("l1"), vec![], 0, 0).unwrap();
        let cancel = CancellationToken::new();

        settings
            .save(&FactorySettings {
                enabled: false,
                ..FactorySettings::default()
            })
            .unwrap();
        assert!(matches!(w.tick(&cancel).await, Tick::Idle(_)));
        settings.save(&FactorySettings::default()).unwrap();

        settings.set_paused(true).unwrap();
        assert!(matches!(w.tick(&cancel).await, Tick::Idle(_)));
        settings.set_paused(false).unwrap();

        *gate.0.lock().unwrap() = Err("owner is talking".into());
        assert!(matches!(w.tick(&cancel).await, Tick::Idle(_)));
        *gate.0.lock().unwrap() = Ok(());

        settings
            .save(&FactorySettings {
                max_model_minutes_per_day: 1,
                ..FactorySettings::default()
            })
            .unwrap();
        // The scripted job takes 90 s of model time, over the 1-minute budget.
        assert!(matches!(w.tick(&cancel).await, Tick::Ran { .. }));
        q.enqueue(lesson("l2"), vec![], 0, 0).unwrap();
        assert!(matches!(w.tick(&cancel).await, Tick::Idle(_)));
        assert_eq!(script.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn blocked_jobs_wait_and_others_run() {
        let (w, q, _, script, _) = worker(vec![]);
        q.enqueue(
            AuthoringJob::PublishCourseRevision {
                course_id: "c".into(),
            },
            vec![PriorityReason::OwnerRequest],
            0,
            0,
        )
        .unwrap();
        q.enqueue(validate("d"), vec![PriorityReason::Enrichment], 0, 0)
            .unwrap();
        let cancel = CancellationToken::new();
        assert!(matches!(w.tick(&cancel).await, Tick::Ran { .. }));
        assert!(matches!(w.tick(&cancel).await, Tick::Idle(_)));
        assert_eq!(*script.seen.lock().unwrap(), vec![validate("d")]);
        assert_eq!(q.active().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn shutdown_defers_instead_of_failing() {
        let (w, q, _, _, _) = worker(vec![Step::outcome(JobOutcome::Failed {
            reason: "interrupted".into(),
            retryable: true,
        })]);
        q.enqueue(validate("d"), vec![], 0, 0).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            w.tick(&cancel).await,
            Tick::Ran {
                outcome: JobOutcome::Defer { .. },
                ..
            }
        ));
        assert_eq!(q.active().unwrap()[0].failures, 0);
    }
}
