//! The course service behind `/api/learn/courses…` (#386). It never calls a model: content
//! comes from authored packages, grades from deterministic graders or the Rust sandbox, and
//! progress from the evidence projection. The server stamps time; clients can't.
use super::content::{CoursePackage, Lesson, RunLimits, Step, StepKind, Track, catalog};
use super::evidence::{
    self, Assistance, AttemptRecord, Confidence, Context, CourseEvent, EvidenceState, Stored,
    rfc3339,
};
use super::grade::{self, Answer, Graded, RustRun, Verdict};
use super::store::{CourseStore, CourseStoreError};
use serde_json::{Value, json};
use std::sync::Arc;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Runs learner Rust in an isolated sandbox. Implementations must never fall back to the
/// host toolchain: no sandbox means [`RustRun::Unavailable`].
#[async_trait::async_trait]
pub trait RustEvaluator: Send + Sync {
    async fn run(&self, source: &str, limits: RunLimits) -> RustRun;
}

/// The evaluator used when no sandbox is configured.
pub struct NoSandbox;

#[async_trait::async_trait]
impl RustEvaluator for NoSandbox {
    async fn run(&self, _source: &str, _limits: RunLimits) -> RustRun {
        RustRun::Unavailable("The isolated Rust runner isn't configured on this host.".into())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CourseError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Store(String),
}

impl From<CourseStoreError> for CourseError {
    fn from(e: CourseStoreError) -> Self {
        Self::Store(e.0)
    }
}

type Result<T> = std::result::Result<T, CourseError>;

/// A learner's submission. The server decides everything else.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRequest {
    #[serde(default)]
    pub step_id: Option<String>,
    pub attempt_id: String,
    #[serde(default)]
    pub answer: Option<Answer>,
    #[serde(default)]
    pub confidence: Option<Confidence>,
}

pub struct CourseEngine {
    store: CourseStore,
    tracks: Vec<Track>,
    clock: Clock,
    rust: Arc<dyn RustEvaluator>,
    /// One writer at a time, so idempotency checks and appends don't interleave.
    write: tokio::sync::Mutex<()>,
}

fn system_clock() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn label<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

impl CourseEngine {
    pub fn new(store: CourseStore, rust: Arc<dyn RustEvaluator>) -> Self {
        Self::with_clock(store, rust, Arc::new(system_clock))
    }

    pub fn with_clock(store: CourseStore, rust: Arc<dyn RustEvaluator>, clock: Clock) -> Self {
        Self {
            store,
            tracks: catalog(),
            clock,
            rust,
            write: tokio::sync::Mutex::new(()),
        }
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn package(&self, course_id: &str) -> Result<&CoursePackage> {
        let track = self
            .tracks
            .iter()
            .find(|t| t.id == course_id)
            .ok_or_else(|| CourseError::NotFound(format!("No course named `{course_id}`.")))?;
        track
            .package
            .as_ref()
            .ok_or_else(|| CourseError::Conflict(format!("{} has no lessons yet.", track.title)))
    }

    fn lesson(&self, lesson_id: &str) -> Result<(&CoursePackage, &Lesson)> {
        self.tracks
            .iter()
            .filter_map(|t| t.package.as_ref())
            .find_map(|p| p.lesson(lesson_id).map(|l| (p, l)))
            .ok_or_else(|| CourseError::NotFound(format!("No lesson named `{lesson_id}`.")))
    }

    fn review(&self, review_id: &str) -> Result<(&CoursePackage, &Step)> {
        self.tracks
            .iter()
            .filter_map(|t| t.package.as_ref())
            .find_map(|p| p.review(review_id).map(|s| (p, s)))
            .ok_or_else(|| CourseError::NotFound(format!("No review named `{review_id}`.")))
    }

    // ---------------------------------------------------------------- catalog and paths

    pub fn courses(&self) -> Result<Value> {
        let events = self.store.events()?;
        let now = self.now();
        let courses: Vec<Value> = self
            .tracks
            .iter()
            .map(|t| self.course_summary(t, &events, now))
            .collect();
        Ok(json!({ "courses": courses }))
    }

    fn course_summary(&self, t: &Track, events: &[Stored], now: i64) -> Value {
        let Some(p) = &t.package else {
            return json!({
                "id": t.id, "title": t.title, "domain": label(&t.domain), "status": "planned",
                "summary": t.summary, "version": null, "lesson_count": 0, "minutes": 0,
                "progress": null, "next": null,
            });
        };
        let ev = evidence::project(p, events, now);
        let count = |s: EvidenceState| ev.iter().filter(|e| e.state == s).count();
        let completed = p
            .lessons
            .iter()
            .filter(|l| lesson_state(l, events) == "completed")
            .count();
        json!({
            "id": p.id, "title": p.title, "domain": label(&p.domain), "status": "ready",
            "summary": p.summary, "version": p.version, "lesson_count": p.lessons.len(),
            "minutes": p.minutes(),
            "progress": {
                "lessons_completed": completed,
                "objectives": p.objectives.len(),
                "provisional": count(EvidenceState::Provisional),
                "retained": count(EvidenceState::Retained),
                "due": evidence::due_queue(p, events, now).total_due,
            },
            "next": self.next_for(p, events, now),
        })
    }

    pub fn course(&self, course_id: &str) -> Result<Value> {
        let track = self
            .tracks
            .iter()
            .find(|t| t.id == course_id)
            .ok_or_else(|| CourseError::NotFound(format!("No course named `{course_id}`.")))?;
        let events = self.store.events()?;
        let now = self.now();
        let summary = self.course_summary(track, &events, now);
        let Some(p) = &track.package else {
            return Ok(
                json!({ "course": summary, "objectives": [], "chapters": [], "diagnostic": null }),
            );
        };
        let chapters: Vec<Value> = p
            .chapters
            .iter()
            .map(|c| {
                let lessons: Vec<Value> = c
                    .lessons
                    .iter()
                    .filter_map(|id| p.lesson(id))
                    .map(|l| {
                        json!({
                            "id": l.id, "title": l.title, "minutes": l.minutes,
                            "objective_ids": l.objective_ids, "prerequisites": l.prerequisites,
                            "state": lesson_state(l, &events),
                            "certified": certified(&p.id, &l.id, &events),
                        })
                    })
                    .collect();
                json!({ "id": c.id, "title": c.title, "lessons": lessons })
            })
            .collect();
        let diagnostic = p.diagnostic.as_ref().map(|d| {
            let last = open_or_last_diagnostic(&p.id, &events);
            let state = match &last {
                None => "not_started",
                Some((_, true)) => "done",
                Some((_, false)) => "in_progress",
            };
            // `session_id` is what `/diagnostics/:id/report` and `…/attempt` take.
            json!({ "id": d.id, "session_id": last.map(|(id, _)| id), "state": state })
        });
        Ok(json!({
            "course": summary,
            "objectives": label(&p.objectives),
            "chapters": chapters,
            "diagnostic": diagnostic,
        }))
    }

    pub fn next(&self, course_id: &str) -> Result<Value> {
        let p = self.package(course_id)?;
        let events = self.store.events()?;
        Ok(self.next_for(p, &events, self.now()))
    }

    fn next_for(&self, p: &CoursePackage, events: &[Stored], now: i64) -> Value {
        if p.diagnostic.is_some()
            && open_or_last_diagnostic(&p.id, events).is_none_or(|(_, done)| !done)
        {
            return json!({
                "kind": "diagnostic",
                "reasons": ["A short cold check finds where to start. Nothing counts as known without a real answer."],
            });
        }
        let queue = evidence::due_queue(p, events, now);
        if queue.blocks() {
            return json!({
                "kind": "review",
                "reasons": [format!("{} objective(s) are due for review before the next certified lesson.", queue.total_due)],
            });
        }
        for l in &p.lessons {
            if certified(&p.id, &l.id, events) {
                continue;
            }
            return json!({
                "kind": "lesson",
                "lesson_id": l.id,
                "reasons": [if lesson_state(l, events) == "not_started" {
                    format!("Next on the path: {}.", l.title)
                } else {
                    format!("Continue {}.", l.title)
                }],
            });
        }
        json!({ "kind": "done", "reasons": ["Every lesson in this course is certified. Reviews will come back when they're due."] })
    }

    // ---------------------------------------------------------------- lessons

    pub fn lesson_view(&self, lesson_id: &str) -> Result<Value> {
        let (p, l) = self.lesson(lesson_id)?;
        let events = self.store.events()?;
        let done = completed_steps(l, &events);
        let current = l
            .steps
            .iter()
            .find(|s| !done.contains(&s.id))
            .map(|s| s.id.clone());
        let steps: Vec<Value> = l
            .steps
            .iter()
            .map(|s| label(&s.view(&l.id, true)))
            .collect();
        Ok(json!({
            "lesson": {
                "id": l.id, "course_id": p.id, "version": p.version, "title": l.title,
                "minutes": l.minutes, "objective_ids": l.objective_ids, "steps": steps,
            },
            "progress": {
                "current_step_id": current,
                "completed_step_ids": l.steps.iter().filter(|s| done.contains(&s.id)).map(|s| &s.id).collect::<Vec<_>>(),
                "state": lesson_state(l, &events),
            },
        }))
    }

    pub async fn lesson_attempt(&self, lesson_id: &str, req: AttemptRequest) -> Result<Value> {
        let (p, l) = self.lesson(lesson_id)?;
        let step_id = req
            .step_id
            .clone()
            .ok_or_else(|| CourseError::Invalid("`step_id` is required.".into()))?;
        let step = l
            .step(&step_id)
            .ok_or_else(|| CourseError::NotFound(format!("No step `{step_id}` in this lesson.")))?;
        self.attempt(p, Context::Lesson, &l.id, step, Some(l), req)
            .await
    }

    pub async fn hint(
        &self,
        context: Context,
        context_id: &str,
        step_id: Option<&str>,
    ) -> Result<Value> {
        let (p, step) = match context {
            Context::Lesson => {
                let (p, l) = self.lesson(context_id)?;
                let id =
                    step_id.ok_or_else(|| CourseError::Invalid("`step_id` is required.".into()))?;
                (
                    p,
                    l.step(id)
                        .ok_or_else(|| CourseError::NotFound(format!("No step `{id}`.")))?,
                )
            }
            Context::Review => self.review(context_id)?,
            Context::Diagnostic => {
                return Err(CourseError::Conflict(
                    "The placement check has no hints.".into(),
                ));
            }
        };
        let _guard = self.write.lock().await;
        let events = self.store.events()?;
        let used = evidence::hints_used(&events, context, context_id, &step.id);
        if usize::from(used) >= step.hints.len() {
            return Err(CourseError::Conflict(
                "There are no more hints for this step.".into(),
            ));
        }
        let level = used + 1;
        self.store.append(
            self.now(),
            &CourseEvent::Hint {
                context,
                context_id: context_id.to_string(),
                course_id: p.id.clone(),
                step_id: step.id.clone(),
                level,
            },
            None,
        )?;
        Ok(json!({
            "level": level,
            "text": step.hints[usize::from(used)],
            "hints_used": level,
            "hint_count": step.hints.len(),
            "assistance_state": "hinted",
        }))
    }

    pub async fn reveal(&self, lesson_id: &str, step_id: &str) -> Result<Value> {
        let (p, l) = self.lesson(lesson_id)?;
        let step = l
            .step(step_id)
            .ok_or_else(|| CourseError::NotFound(format!("No step `{step_id}`.")))?;
        if !step.scored() {
            return Err(CourseError::Invalid(
                "This step has no solution to reveal.".into(),
            ));
        }
        let _guard = self.write.lock().await;
        self.store.append(
            self.now(),
            &CourseEvent::Reveal {
                context: Context::Lesson,
                context_id: l.id.clone(),
                course_id: p.id.clone(),
                step_id: step.id.clone(),
            },
            None,
        )?;
        Ok(json!({ "solution": label(&step.solution), "assistance_state": "solution_seen" }))
    }

    /// The certified-advance gate. Enforced here, never in the client.
    pub async fn advance(&self, lesson_id: &str) -> Result<Value> {
        let (p, l) = self.lesson(lesson_id)?;
        let _guard = self.write.lock().await;
        let events = self.store.events()?;
        let now = self.now();
        if lesson_state(l, &events) != "completed" {
            return Err(CourseError::Conflict(
                "Finish every step of this lesson first.".into(),
            ));
        }
        let queue = evidence::due_queue(p, &events, now);
        let due = self.review_items(p, &queue);
        if queue.blocks() {
            let mut reasons = vec![format!(
                "Review required before the next certified unit: {} objective(s) are due. You can still read any lesson.",
                queue.total_due
            )];
            reasons.extend(
                queue
                    .items
                    .iter()
                    .map(|i| format!("{}: {}", i.objective_title, i.reason)),
            );
            return Ok(
                json!({ "gate_decision": "blocked_by_review", "gate_reasons": reasons, "certified": false, "due": due }),
            );
        }
        if certified(&p.id, &l.id, &events) {
            return Ok(
                json!({ "gate_decision": "continue", "gate_reasons": ["This lesson is already certified."], "certified": true, "due": due }),
            );
        }
        let projected = evidence::project(p, &events, now);
        let mut reasons = Vec::new();
        for o in &l.objective_ids {
            if let Some(e) = projected.iter().find(|e| &e.objective_id == o)
                && e.independent_passes == 0
            {
                reasons.push(format!(
                    "{}: not yet passed without help. A fresh problem in review can count it.",
                    e.title
                ));
            }
        }
        if !reasons.is_empty() {
            return Ok(
                json!({ "gate_decision": "continue", "gate_reasons": reasons, "certified": false, "due": due }),
            );
        }
        self.store.append(
            now,
            &CourseEvent::LessonCertified {
                course_id: p.id.clone(),
                lesson_id: l.id.clone(),
                course_version: p.version.clone(),
            },
            None,
        )?;
        Ok(json!({
            "gate_decision": "continue",
            "gate_reasons": ["Certified as passed independently today. It stays provisional until a delayed check on a new problem."],
            "certified": true,
            "due": due,
        }))
    }

    // ---------------------------------------------------------------- diagnostics

    pub async fn diagnostic_start(&self, course_id: &str) -> Result<Value> {
        let p = self.package(course_id)?;
        if p.diagnostic.is_none() {
            return Err(CourseError::Conflict(
                "This course has no placement check.".into(),
            ));
        }
        let _guard = self.write.lock().await;
        let events = self.store.events()?;
        let id = match open_or_last_diagnostic(&p.id, &events) {
            Some((id, false)) => id,
            _ => {
                let id = format!("diag-{}", uuid::Uuid::new_v4().simple());
                self.store.append(
                    self.now(),
                    &CourseEvent::DiagnosticStarted {
                        id: id.clone(),
                        course_id: p.id.clone(),
                    },
                    None,
                )?;
                id
            }
        };
        let events = self.store.events()?;
        let next = self.diagnostic_next(p, &id, &events)?;
        Ok(
            json!({ "id": id, "step": next.map(|s| label(&s.view(&id, false))), "done": next.is_none() }),
        )
    }

    fn diagnostic_course(&self, id: &str) -> Result<&CoursePackage> {
        let events = self.store.events()?;
        let course = events
            .iter()
            .find_map(|s| match &s.event {
                CourseEvent::DiagnosticStarted { id: d, course_id } if d == id => {
                    Some(course_id.clone())
                }
                _ => None,
            })
            .ok_or_else(|| CourseError::NotFound(format!("No placement check `{id}`.")))?;
        self.package(&course)
    }

    /// The next unanswered item. An item is skipped (and stays untested) when a
    /// prerequisite objective was answered wrongly earlier in this check.
    fn diagnostic_next<'a>(
        &self,
        p: &'a CoursePackage,
        id: &str,
        events: &[Stored],
    ) -> Result<Option<&'a Step>> {
        let Some(d) = &p.diagnostic else {
            return Ok(None);
        };
        let mut answered = std::collections::BTreeSet::new();
        let mut failed = std::collections::BTreeSet::new();
        for s in events {
            if let CourseEvent::Attempt(a) = &s.event
                && a.context == Context::Diagnostic
                && a.context_id == id
            {
                answered.insert(a.step_id.clone());
                if a.verdict == Verdict::Incorrect {
                    failed.extend(a.objective_ids.iter().cloned());
                }
            }
        }
        Ok(d.steps.iter().find(|s| {
            !answered.contains(&s.id)
                && !s.objective_ids.iter().any(|o| {
                    p.objective(o)
                        .is_some_and(|obj| obj.prerequisites.iter().any(|pre| failed.contains(pre)))
                })
        }))
    }

    pub async fn diagnostic_attempt(&self, id: &str, req: AttemptRequest) -> Result<Value> {
        let p = self.diagnostic_course(id)?;
        if let Some(prior) = self.replay(&req.attempt_id)? {
            return Ok(prior);
        }
        let events = self.store.events()?;
        if events
            .iter()
            .any(|s| matches!(&s.event, CourseEvent::DiagnosticFinished { id: d } if d == id))
        {
            return Err(CourseError::Conflict(
                "This placement check is finished.".into(),
            ));
        }
        let step = self.diagnostic_next(p, id, &events)?.ok_or_else(|| {
            CourseError::Conflict("This placement check has no open question.".into())
        })?;
        if req.step_id.as_deref() != Some(step.id.as_str()) {
            return Err(CourseError::Conflict(format!(
                "Answer the current question, `{}`.",
                step.id
            )));
        }
        let mut grade = self
            .attempt(p, Context::Diagnostic, id, step, None, req)
            .await?;
        let _guard = self.write.lock().await;
        let events = self.store.events()?;
        let next = self.diagnostic_next(p, id, &events)?;
        if next.is_none() {
            self.store.append(
                self.now(),
                &CourseEvent::DiagnosticFinished { id: id.to_string() },
                None,
            )?;
        }
        grade["next"] = next
            .map(|s| label(&s.view(id, false)))
            .unwrap_or(Value::Null);
        grade["done"] = json!(next.is_none());
        grade["gate_decision"] = json!(if next.is_none() {
            "lesson_complete"
        } else {
            "continue"
        });
        grade["next_step_id"] = next.map(|s| json!(s.id)).unwrap_or(Value::Null);
        Ok(grade)
    }

    pub fn diagnostic_report(&self, id: &str) -> Result<Value> {
        let p = self.diagnostic_course(id)?;
        let events = self.store.events()?;
        let projected = evidence::project(p, &events, self.now());
        let untested: Vec<&str> = projected
            .iter()
            .filter(|e| e.not_assessed)
            .map(|e| e.objective_id.as_str())
            .collect();
        let independent = projected
            .iter()
            .filter(|e| e.independent_passes > 0)
            .count();
        let summary = format!(
            "{} of {} objectives have a checked answer; {} passed without help. Untested objectives stay untested: SAIL doesn't guess.",
            projected.len() - untested.len(),
            projected.len(),
            independent
        );
        Ok(json!({ "objectives": label(&projected), "untested": untested, "summary": summary }))
    }

    // ---------------------------------------------------------------- reviews and evidence

    fn review_items(&self, p: &CoursePackage, queue: &evidence::DueQueue) -> Vec<Value> {
        queue
            .items
            .iter()
            .filter_map(|i| {
                let step = p.review(&i.step_id)?;
                Some(json!({
                    "id": i.step_id,
                    "objective_id": i.objective_id,
                    "objective_title": i.objective_title,
                    "due_at": rfc3339(i.due_ms),
                    "reason": i.reason,
                    "step": label(&step.view(&p.id, true)),
                }))
            })
            .collect()
    }

    pub fn reviews_due(&self, course_id: &str) -> Result<Value> {
        let p = self.package(course_id)?;
        let events = self.store.events()?;
        let queue = evidence::due_queue(p, &events, self.now());
        Ok(json!({
            "items": self.review_items(p, &queue),
            "catch_up": {
                "total_due": queue.total_due,
                "cap": evidence::REVIEW_CAP,
                "sessions": queue.sessions(),
                "reviewed_today": queue.reviewed_recently,
            },
            "policy_version": evidence::POLICY_VERSION,
        }))
    }

    pub async fn review_attempt(&self, review_id: &str, req: AttemptRequest) -> Result<Value> {
        let (p, step) = self.review(review_id)?;
        if let Some(prior) = self.replay(&req.attempt_id)? {
            return Ok(prior);
        }
        let events = self.store.events()?;
        let queue = evidence::due_queue(p, &events, self.now());
        if !queue.items.iter().any(|i| i.step_id == step.id) {
            return Err(CourseError::Conflict(
                "This review isn't due right now.".into(),
            ));
        }
        self.attempt(p, Context::Review, &step.id, step, None, req)
            .await
    }

    pub fn evidence(&self, course_id: &str) -> Result<Value> {
        let p = self.package(course_id)?;
        let events = self.store.events()?;
        Ok(json!({
            "objectives": label(&evidence::project(p, &events, self.now())),
            "policy_version": evidence::POLICY_VERSION,
        }))
    }

    // ---------------------------------------------------------------- shared attempt path

    fn replay(&self, attempt_id: &str) -> Result<Option<Value>> {
        Ok(match self.store.by_attempt_key(attempt_id)? {
            Some(Stored {
                event: CourseEvent::Attempt(a),
                ..
            }) => Some(a.response),
            Some(_) => Some(json!({ "result": "correct", "replayed": true })),
            None => None,
        })
    }

    async fn attempt(
        &self,
        p: &CoursePackage,
        context: Context,
        context_id: &str,
        step: &Step,
        lesson: Option<&Lesson>,
        req: AttemptRequest,
    ) -> Result<Value> {
        let id = req.attempt_id.trim();
        if id.is_empty()
            || id.len() > 64
            || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(CourseError::Invalid(
                "`attempt_id` must be 1–64 letters, digits or dashes.".into(),
            ));
        }
        let _guard = self.write.lock().await;
        if let Some(prior) = self.replay(id)? {
            return Ok(prior);
        }
        let now = self.now();
        let mut events = self.store.events()?;
        let revision = self.store.last_seq()? + 1;

        if step.kind == StepKind::Explain {
            if req.answer.is_some() {
                return Err(CourseError::Invalid(
                    "An explanation step takes `answer: null`.".into(),
                ));
            }
            let event = CourseEvent::Exposure {
                course_id: p.id.clone(),
                lesson_id: context_id.to_string(),
                step_id: step.id.clone(),
                objective_ids: step.objective_ids.clone(),
            };
            self.store.append(now, &event, Some(id))?;
            events.push(Stored {
                seq: revision,
                at_ms: now,
                event,
            });
            let next = lesson.and_then(|l| next_step(l, &events));
            return Ok(json!({
                "result": "correct", "evidence_id": format!("exp-{id}"),
                "skill_ids": step.objective_ids, "validator_kind": "choice",
                "assistance_state": "independent", "provisional_vs_retained": "none",
                "review_due_at": null,
                "gate_decision": if next.is_some() { "continue" } else { "lesson_complete" },
                "gate_reasons": [], "revision": revision,
                "feedback": { "summary": "Read.", "explanation": [], "diagnostics": [] },
                "next_step_id": next,
            }));
        }

        let answer = req
            .answer
            .as_ref()
            .ok_or_else(|| CourseError::Invalid("`answer` is required for this step.".into()))?;
        let graded: Graded = if step.kind == StepKind::RustEdit {
            let Answer::Code(code) = answer else {
                return Err(CourseError::Invalid(
                    "This step takes `{\"code\": …}`.".into(),
                ));
            };
            if code.len() > grade::MAX_CODE {
                return Err(CourseError::Invalid(format!(
                    "Code is limited to {} bytes.",
                    grade::MAX_CODE
                )));
            }
            let limits = step.limits.unwrap_or(RunLimits {
                wall_ms: 20_000,
                mem_mb: 512,
            });
            let run = self.rust.run(code, limits).await;
            grade::grade_rust(step, &run).map_err(CourseError::Invalid)?
        } else {
            grade::grade(step, answer).map_err(CourseError::Invalid)?
        };
        let assistance = if context == Context::Diagnostic {
            Assistance::Independent
        } else {
            evidence::assistance_for(&events, context, context_id, &step.id)
        };

        let evidence_id = format!("ev-{}", uuid::Uuid::new_v4().simple());
        let mut record = AttemptRecord {
            evidence_id: evidence_id.clone(),
            attempt_id: id.to_string(),
            context,
            context_id: context_id.to_string(),
            course_id: p.id.clone(),
            course_version: p.version.clone(),
            step_id: step.id.clone(),
            family: step.family.clone(),
            objective_ids: step.objective_ids.clone(),
            verdict: graded.verdict,
            validator: graded.validator,
            assistance,
            confidence: req.confidence,
            response: Value::Null,
        };
        events.push(Stored {
            seq: revision,
            at_ms: now,
            event: CourseEvent::Attempt(record.clone()),
        });
        let projected = evidence::project(p, &events, now);
        let mine: Vec<_> = projected
            .iter()
            .filter(|e| step.objective_ids.contains(&e.objective_id))
            .collect();
        let standing = if mine.iter().all(|e| e.state == EvidenceState::Retained) {
            "retained"
        } else if mine.iter().all(|e| e.independent_passes > 0) {
            "provisional"
        } else {
            "none"
        };
        let review_due = mine.iter().filter_map(|e| e.due_ms).min().map(rfc3339);
        let next = lesson.and_then(|l| next_step(l, &events));
        let (decision, mut reasons) = gate_for(context, graded.verdict, next.is_some());
        if graded.verdict == Verdict::Correct && assistance != Assistance::Independent {
            reasons.push(
                "You used help, so this counts as assisted. A fresh problem will check it later."
                    .into(),
            );
        }
        if context == Context::Review
            && graded.verdict == Verdict::Correct
            && assistance == Assistance::Independent
            && standing != "retained"
        {
            reasons.push(
                "Passed. It counts as retained once it holds on a new problem after a delay."
                    .into(),
            );
        }
        let response = json!({
            "result": label(&graded.verdict),
            "evidence_id": evidence_id,
            "skill_ids": step.objective_ids,
            "validator_kind": label(&graded.validator),
            "assistance_state": label(&assistance),
            "provisional_vs_retained": standing,
            "review_due_at": review_due,
            "gate_decision": decision,
            "gate_reasons": reasons,
            "revision": revision,
            "feedback": label(&graded.feedback),
            "next_step_id": next,
            "policy_version": evidence::POLICY_VERSION,
        });
        record.response = response.clone();
        self.store
            .append(now, &CourseEvent::Attempt(record), Some(id))?;
        Ok(response)
    }
}

fn gate_for(context: Context, verdict: Verdict, has_next: bool) -> (&'static str, Vec<String>) {
    match (context, verdict) {
        (_, Verdict::Unavailable) => (
            if has_next || context != Context::Lesson { "continue" } else { "lesson_complete" },
            vec!["Evaluation is unavailable, so this step isn't scored. You can continue; it isn't marked wrong.".into()],
        ),
        (Context::Lesson, Verdict::Incorrect) => ("retry", vec![]),
        (Context::Lesson, Verdict::Correct) if !has_next => (
            "lesson_complete",
            vec!["Lesson finished. Advance to check whether it can be certified.".into()],
        ),
        _ => ("continue", vec![]),
    }
}

/// Steps done in a lesson: explanations read, steps passed, and steps whose evaluator was
/// unavailable (shown as not assessed rather than blocking the lesson).
fn completed_steps(l: &Lesson, events: &[Stored]) -> std::collections::BTreeSet<String> {
    let mut done = std::collections::BTreeSet::new();
    for s in events {
        match &s.event {
            CourseEvent::Exposure {
                lesson_id, step_id, ..
            } if lesson_id == &l.id => {
                done.insert(step_id.clone());
            }
            CourseEvent::Attempt(a)
                if a.context == Context::Lesson
                    && a.context_id == l.id
                    && matches!(a.verdict, Verdict::Correct | Verdict::Unavailable) =>
            {
                done.insert(a.step_id.clone());
            }
            _ => {}
        }
    }
    done
}

fn next_step(l: &Lesson, events: &[Stored]) -> Option<String> {
    let done = completed_steps(l, events);
    l.steps
        .iter()
        .find(|s| !done.contains(&s.id))
        .map(|s| s.id.clone())
}

fn lesson_state(l: &Lesson, events: &[Stored]) -> &'static str {
    let done = completed_steps(l, events);
    let touched = !done.is_empty()
        || events.iter().any(|s| matches!(&s.event, CourseEvent::Attempt(a) if a.context == Context::Lesson && a.context_id == l.id));
    if l.steps.iter().all(|s| done.contains(&s.id)) {
        "completed"
    } else if touched {
        "in_progress"
    } else {
        "not_started"
    }
}

fn certified(course_id: &str, lesson_id: &str, events: &[Stored]) -> bool {
    events.iter().any(|s| {
        matches!(&s.event, CourseEvent::LessonCertified { course_id: c, lesson_id: l, .. } if c == course_id && l == lesson_id)
    })
}

/// The open placement check for a course, else the last finished one: `(id, finished)`.
fn open_or_last_diagnostic(course_id: &str, events: &[Stored]) -> Option<(String, bool)> {
    let mut last: Option<(String, bool)> = None;
    for s in events {
        match &s.event {
            CourseEvent::DiagnosticStarted { id, course_id: c } if c == course_id => {
                last = Some((id.clone(), false));
            }
            CourseEvent::DiagnosticFinished { id } => {
                if let Some((open, done)) = &mut last
                    && open == id
                {
                    *done = true;
                }
            }
            _ => {}
        }
    }
    last
}
