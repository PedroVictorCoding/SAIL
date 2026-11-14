//! Durable jobs, runs and the pure planner behind the scheduler.
//!
//! The model and the owner both create jobs here. A model-made job starts as
//! [`JobState::Proposed`] and never runs until the owner approves it. Files are written
//! atomically under one process-wide lock, so a tick and an owner action never
//! interleave their reads and writes.
use crate::schedule::when::{next_after, parse};
use crate::skills::valid_name;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};

/// The most jobs a store may hold.
pub const MAX_JOBS: usize = 100;
/// The most jobs made by the model, in any state.
pub const MAX_MODEL_JOBS: usize = 10;
/// Failed runs in a row before the job is paused and the owner is told.
pub const FAILURES_BEFORE_PAUSE: u32 = 3;

const RUNS_PER_JOB: usize = 50;
const MAX_SUMMARY_CHARS: usize = 2000;
const MAX_REASON_CHARS: usize = 500;

static STORE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn lock_store() -> MutexGuard<'static, ()> {
    match STORE_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// One schedule as the owner sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Job {
    /// 32 lowercase hex characters, given by the caller.
    pub id: String,
    /// 1–80 characters, trimmed, without control characters.
    pub name: String,
    /// What the owner or the model typed.
    pub when_text: String,
    /// IANA zone the schedule is evaluated in.
    pub tz: String,
    /// Creation time; also the base for relative schedules.
    pub anchor: i64,
    /// What to run.
    pub action: Action,
    /// Where the result goes.
    pub deliver: Deliver,
    /// Current lifecycle state.
    pub state: JobState,
    /// Who made the job.
    pub created_by: Creator,
    /// Unix seconds.
    pub created_at: i64,
    /// The last slot started or recorded as missed.
    pub last_slot: Option<i64>,
    /// Consecutive failed runs.
    pub failure_streak: u32,
    /// Why the model proposed the job (model jobs only, at most 500 characters).
    pub reason: Option<String>,
}

/// What a due slot runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Send a prompt to the agent.
    Prompt {
        /// 1–4000 characters.
        text: String,
    },
    /// Run an installed Agent Skill.
    Skill {
        /// Must pass [`crate::skills::valid_name`].
        name: String,
        /// Up to 4000 characters.
        input: String,
    },
    /// Run a workflow by id.
    Workflow {
        /// 1–128 characters of `[A-Za-z0-9_-]`.
        id: String,
    },
}

/// Where a run's result is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deliver {
    /// Show it in the web UI when quiet hours allow.
    Web,
    /// Do not show it.
    Silent,
}

/// Who created a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Creator {
    /// The owner.
    Owner,
    /// The model; such jobs need approval.
    Model,
}

/// Lifecycle of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Runs on its slots.
    Active,
    /// Does not run until resumed.
    Paused,
    /// Made by the model and waiting for the owner; it never runs.
    Proposed,
    /// A `Once` job that has run.
    Done,
}

/// One execution of one slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Run {
    /// `"<job_id>:<slot>"`.
    pub id: String,
    /// The job this run belongs to.
    pub job_id: String,
    /// The unix second the run represents.
    pub slot: i64,
    /// When the runner started it.
    pub started_at: Option<i64>,
    /// When it finished.
    pub finished_at: Option<i64>,
    /// Current or final outcome.
    pub outcome: Outcome,
    /// At most 2000 characters, cut on a char boundary with `…`.
    pub summary: String,
    /// When a web result was shown; `None` while quiet hours hold it.
    pub delivered_at: Option<i64>,
    /// True when the run is late.
    pub catch_up: bool,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Still executing.
    Running,
    /// Finished successfully.
    Ok,
    /// Finished with an error.
    Failed,
    /// The slot passed while SAIL was down.
    Missed,
}

/// One slot the runner must start now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Due {
    /// The job to run.
    pub job_id: String,
    /// The slot to run.
    pub slot: i64,
    /// True when the slot is more than a minute old.
    pub catch_up: bool,
}

/// What one tick must do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Plan {
    /// Slots to start now, at most one per job.
    pub due: Vec<Due>,
    /// Collapsed runs for slots that passed outside the grace window.
    pub missed: Vec<Run>,
}

/// Persistent job and run storage rooted at `dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// The directory holding `jobs.json` and `runs.json`.
    pub dir: PathBuf,
}

impl Store {
    /// Read every job. A missing file is an empty store.
    pub fn jobs(&self) -> Result<Vec<Job>, String> {
        let _guard = lock_store();
        load_jobs(&self.dir)
    }

    /// Read one job.
    pub fn get(&self, id: &str) -> Result<Job, String> {
        let _guard = lock_store();
        load_jobs(&self.dir)?
            .into_iter()
            .find(|job| job.id == id)
            .ok_or_else(|| format!("no job with id {id}"))
    }

    /// Validate and add a job, enforcing every limit and a unique id and name.
    pub fn create(&self, job: Job) -> Result<Job, String> {
        let _guard = lock_store();
        let mut jobs = load_jobs(&self.dir)?;
        let job = validate(job, &jobs, None)?;
        jobs.push(job.clone());
        save_jobs(&self.dir, &jobs)?;
        Ok(job)
    }

    /// Validate and replace an existing job.
    pub fn update(&self, job: &Job) -> Result<(), String> {
        let _guard = lock_store();
        let mut jobs = load_jobs(&self.dir)?;
        let index = jobs
            .iter()
            .position(|existing| existing.id == job.id)
            .ok_or_else(|| format!("no job with id {}", job.id))?;
        let job = validate(job.clone(), &jobs, Some(index))?;
        jobs[index] = job;
        save_jobs(&self.dir, &jobs)
    }

    /// Delete a job and its runs.
    pub fn delete(&self, id: &str) -> Result<(), String> {
        let _guard = lock_store();
        let mut jobs = load_jobs(&self.dir)?;
        let before = jobs.len();
        jobs.retain(|job| job.id != id);
        if jobs.len() == before {
            return Err(format!("no job with id {id}"));
        }
        let mut runs = load_runs(&self.dir)?;
        runs.retain(|run| run.job_id != id);
        save_runs(&self.dir, runs)?;
        save_jobs(&self.dir, &jobs)
    }

    /// Record a running slot and advance the job's `last_slot` in one critical section.
    pub fn start(&self, due: &Due, now: i64) -> Result<Run, String> {
        let _guard = lock_store();
        let mut jobs = load_jobs(&self.dir)?;
        let index = jobs
            .iter()
            .position(|job| job.id == due.job_id)
            .ok_or_else(|| format!("no job with id {}", due.job_id))?;
        let id = format!("{}:{}", due.job_id, due.slot);
        let mut runs = load_runs(&self.dir)?;
        if runs.iter().any(|run| run.id == id) {
            return Err(format!("run {id} already started"));
        }
        let run = Run {
            id,
            job_id: due.job_id.clone(),
            slot: due.slot,
            started_at: Some(now),
            finished_at: None,
            outcome: Outcome::Running,
            summary: String::new(),
            delivered_at: None,
            catch_up: due.catch_up,
        };
        runs.push(run.clone());
        trim_runs(&mut runs);
        jobs[index].last_slot = Some(due.slot);
        save_runs(&self.dir, runs)?;
        save_jobs(&self.dir, &jobs)?;
        Ok(run)
    }

    /// Record a missed slot and advance the job to the run's slot.
    pub fn record_missed(&self, run: &Run) -> Result<(), String> {
        let _guard = lock_store();
        let mut jobs = load_jobs(&self.dir)?;
        let index = jobs
            .iter()
            .position(|job| job.id == run.job_id)
            .ok_or_else(|| format!("no job with id {}", run.job_id))?;
        let mut runs = load_runs(&self.dir)?;
        runs.retain(|existing| existing.id != run.id);
        runs.push(run.clone());
        trim_runs(&mut runs);
        jobs[index].last_slot = Some(run.slot);
        save_runs(&self.dir, runs)?;
        save_jobs(&self.dir, &jobs)
    }

    /// Finish a running run, update its job and return the run plus "tell the owner".
    pub fn finish_run(
        &self,
        run_id: &str,
        outcome: Outcome,
        summary: &str,
        now: i64,
    ) -> Result<(Run, bool), String> {
        if outcome == Outcome::Running {
            return Err("a run cannot finish as running".into());
        }
        let _guard = lock_store();
        let mut runs = load_runs(&self.dir)?;
        let run_index = runs
            .iter()
            .position(|run| run.id == run_id)
            .ok_or_else(|| format!("no run with id {run_id}"))?;
        if runs[run_index].outcome != Outcome::Running {
            return Err(format!("run {run_id} already finished"));
        }
        let mut jobs = load_jobs(&self.dir)?;
        let job_index = jobs
            .iter()
            .position(|job| job.id == runs[run_index].job_id)
            .ok_or_else(|| format!("no job for run {run_id}"))?;
        let notify = finish(&mut jobs[job_index], &outcome);
        runs[run_index].outcome = outcome;
        runs[run_index].finished_at = Some(now);
        runs[run_index].summary = cut(summary, MAX_SUMMARY_CHARS);
        let run = runs[run_index].clone();
        save_runs(&self.dir, runs)?;
        save_jobs(&self.dir, &jobs)?;
        Ok((run, notify))
    }

    /// Record that a run's result was shown.
    pub fn mark_delivered(&self, run_id: &str, now: i64) -> Result<(), String> {
        let _guard = lock_store();
        let mut runs = load_runs(&self.dir)?;
        let run = runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .ok_or_else(|| format!("no run with id {run_id}"))?;
        if run.delivered_at.is_none() {
            run.delivered_at = Some(now);
        }
        save_runs(&self.dir, runs)
    }

    /// A job's runs, newest first.
    pub fn runs(&self, job_id: &str) -> Result<Vec<Run>, String> {
        let _guard = lock_store();
        let mut runs: Vec<Run> = load_runs(&self.dir)?
            .into_iter()
            .filter(|run| run.job_id == job_id)
            .collect();
        runs.sort_by(|left, right| {
            right
                .slot
                .cmp(&left.slot)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(runs)
    }

    /// Turn runs left `Running` by a crash into failed runs; returns how many changed.
    pub fn recover(&self, now: i64) -> Result<usize, String> {
        let _guard = lock_store();
        let mut runs = load_runs(&self.dir)?;
        let mut jobs = load_jobs(&self.dir)?;
        let mut changed = 0;
        for run in runs
            .iter_mut()
            .filter(|run| run.outcome == Outcome::Running)
        {
            run.outcome = Outcome::Failed;
            run.finished_at = Some(now);
            run.summary = "SAIL stopped during this run".to_string();
            if let Some(job) = jobs.iter_mut().find(|job| job.id == run.job_id) {
                finish(job, &Outcome::Failed);
            }
            changed += 1;
        }
        if changed > 0 {
            save_runs(&self.dir, runs)?;
            save_jobs(&self.dir, &jobs)?;
        }
        Ok(changed)
    }
}

/// The pure planner. See the round-21 contract for the grace and collapse rules.
pub fn plan(jobs: &[Job], now: i64, grace_secs: i64) -> Plan {
    let mut due = Vec::new();
    let mut missed = Vec::new();
    for job in jobs {
        if job.state != JobState::Active {
            continue;
        }
        let Ok(spec) = parse(&job.when_text, &job.tz, job.anchor) else {
            continue;
        };
        let base = job.last_slot.unwrap_or(job.anchor.saturating_sub(1));
        let mut cursor = base;
        let mut first = None;
        let mut previous = None;
        let mut last = None;
        let mut count = 0usize;
        while let Some(slot) = next_after(&spec, cursor) {
            if slot <= cursor || slot > now {
                break;
            }
            if first.is_none() {
                first = Some(slot);
            }
            previous = last;
            last = Some(slot);
            count += 1;
            cursor = slot;
            if count > 1_000_000 {
                break;
            }
        }
        let (Some(first), Some(latest)) = (first, last) else {
            continue;
        };
        let in_grace = now.saturating_sub(latest) <= grace_secs;
        if in_grace {
            if count > 1
                && let Some(last_missed) = previous
            {
                missed.push(missed_run(job, first, last_missed, count - 1));
            }
            due.push(Due {
                job_id: job.id.clone(),
                slot: latest,
                catch_up: now.saturating_sub(latest) > 60,
            });
        } else {
            missed.push(missed_run(job, first, latest, count));
        }
    }
    due.sort_by(|left, right| {
        left.slot
            .cmp(&right.slot)
            .then_with(|| left.job_id.cmp(&right.job_id))
    });
    missed.sort_by(|left, right| {
        left.slot
            .cmp(&right.slot)
            .then_with(|| left.job_id.cmp(&right.job_id))
    });
    Plan { due, missed }
}

fn missed_run(job: &Job, first: i64, last: i64, count: usize) -> Run {
    Run {
        id: format!("{}:{last}", job.id),
        job_id: job.id.clone(),
        slot: last,
        started_at: None,
        finished_at: None,
        outcome: Outcome::Missed,
        summary: format!(
            "missed {count} runs between {} and {}",
            iso(first),
            iso(last)
        ),
        delivered_at: None,
        catch_up: false,
    }
}

/// The pure job transition after a finished run.
pub fn finish(job: &mut Job, outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Running => return false,
        Outcome::Ok => job.failure_streak = 0,
        Outcome::Failed => {
            job.failure_streak = job.failure_streak.saturating_add(1);
            if job.failure_streak >= FAILURES_BEFORE_PAUSE && job.state == JobState::Active {
                job.state = JobState::Paused;
                return true;
            }
        }
        Outcome::Missed => {}
    }
    if matches!(job.state, JobState::Active | JobState::Paused) {
        let parsed = parse(&job.when_text, &job.tz, job.anchor);
        if parsed.is_ok_and(|spec| matches!(spec.when, crate::schedule::when::When::Once { .. })) {
            job.state = JobState::Done;
        }
    }
    false
}

fn validate(job: Job, jobs: &[Job], updating: Option<usize>) -> Result<Job, String> {
    let mut job = job;
    job.name = job.name.trim().to_string();
    if !valid_id(&job.id) {
        return Err("the job id must be 32 lowercase hex characters".into());
    }
    let name_chars = job.name.chars().count();
    if name_chars == 0 || name_chars > 80 {
        return Err("the job name must be 1 to 80 characters".into());
    }
    if job.name.chars().any(char::is_control) {
        return Err("the job name cannot contain control characters".into());
    }
    if let Some(reason) = &job.reason
        && reason.chars().count() > MAX_REASON_CHARS
    {
        return Err("the reason must be at most 500 characters".into());
    }
    if job.created_by == Creator::Model
        && job
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .is_none()
    {
        return Err("a job made by the model needs a reason".into());
    }
    parse(&job.when_text, &job.tz, job.anchor).map_err(|error| format!("schedule: {error}"))?;
    validate_action(&job.action)?;
    let key = job.name.to_lowercase();
    if jobs
        .iter()
        .enumerate()
        .any(|(index, existing)| Some(index) != updating && existing.name.to_lowercase() == key)
    {
        return Err(format!("a job named {} already exists", job.name));
    }
    if updating.is_none() && jobs.len() >= MAX_JOBS {
        return Err("the schedule holds at most 100 jobs".into());
    }
    if job.created_by == Creator::Model {
        let model_jobs = jobs
            .iter()
            .enumerate()
            .filter(|(index, existing)| {
                Some(*index) != updating && existing.created_by == Creator::Model
            })
            .count();
        if model_jobs >= MAX_MODEL_JOBS {
            return Err("SAIL can propose at most 10 jobs".into());
        }
    }
    Ok(job)
}

fn validate_action(action: &Action) -> Result<(), String> {
    match action {
        Action::Prompt { text } => {
            let chars = text.chars().count();
            if chars == 0 || chars > 4000 {
                return Err("a prompt must be 1 to 4000 characters".into());
            }
        }
        Action::Skill { name, input } => {
            if !valid_name(name) {
                return Err(format!("\"{name}\" is not a valid skill name"));
            }
            if input.chars().count() > 4000 {
                return Err("skill input must be at most 4000 characters".into());
            }
        }
        Action::Workflow { id } => {
            let chars = id.chars().count();
            if chars == 0 || chars > 128 {
                return Err("a workflow id must be 1 to 128 characters".into());
            }
            if !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err("a workflow id may only use letters, digits, _ and -".into());
            }
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Serialize, Deserialize)]
struct JobFile {
    version: u32,
    jobs: Vec<Job>,
}

#[derive(Serialize, Deserialize)]
struct RunFile {
    version: u32,
    runs: Vec<Run>,
}

fn load_jobs(dir: &Path) -> Result<Vec<Job>, String> {
    match fs::read(dir.join("jobs.json")) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("cannot read jobs: {error}")),
        Ok(bytes) => {
            let file: JobFile =
                serde_json::from_slice(&bytes).map_err(|error| format!("corrupt jobs: {error}"))?;
            if file.version != 1 {
                return Err(format!("unsupported jobs version {}", file.version));
            }
            Ok(file.jobs)
        }
    }
}

fn load_runs(dir: &Path) -> Result<Vec<Run>, String> {
    match fs::read(dir.join("runs.json")) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("cannot read runs: {error}")),
        Ok(bytes) => {
            let file: RunFile =
                serde_json::from_slice(&bytes).map_err(|error| format!("corrupt runs: {error}"))?;
            if file.version != 1 {
                return Err(format!("unsupported runs version {}", file.version));
            }
            Ok(file.runs)
        }
    }
}

fn save_jobs(dir: &Path, jobs: &[Job]) -> Result<(), String> {
    write_json(
        &dir.join("jobs.json"),
        &JobFile {
            version: 1,
            jobs: jobs.to_vec(),
        },
    )
}

fn save_runs(dir: &Path, mut runs: Vec<Run>) -> Result<(), String> {
    trim_runs(&mut runs);
    write_json(&dir.join("runs.json"), &RunFile { version: 1, runs })
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = File::create(&tmp).map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    fs::rename(&tmp, path).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn trim_runs(runs: &mut Vec<Run>) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut kept = Vec::with_capacity(runs.len());
    for run in runs.drain(..).rev() {
        let slot = counts.entry(run.job_id.clone()).or_default();
        if *slot < RUNS_PER_JOB {
            *slot += 1;
            kept.push(run);
        }
    }
    kept.reverse();
    *runs = kept;
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn iso(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|value| value.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| seconds.to_string())
}
