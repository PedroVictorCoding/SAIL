//! SAIL's scheduler (P3, round 21): runs the owner's scheduled jobs.
//!
//! Jobs, runs and the planner are `sail_flow::schedule`. This module is the daemon side:
//!
//! * **Tick.** Every 30 s the planner says which slots are due. Each due slot is first
//!   recorded as started (`Store::start`, which also moves the job's `last_slot`), so a
//!   restart or a second tick can never run the same slot twice; runs left unfinished by
//!   a crash are marked failed on start-up (`Store::recover`). Slots missed while SAIL was
//!   down are run once if the latest is within the 2-hour grace window, and recorded as
//!   missed otherwise.
//! * **Attention.** Nothing starts while the owner is talking to SAIL (live voice). Quiet
//!   hours hold the *delivery*, not the run: the result waits and is delivered when quiet
//!   hours end. Delivery is an item in Pulse's inbox (attention store) linking to the
//!   Schedule section; every result is also kept in the job's run history.
//! * **Safety.** A run has nobody present: tools that need approval are not approved (the
//!   run reports which ones it could not use), and a run cannot create scheduled jobs
//!   ([`in_scheduled_run`]). Three failed runs in a row pause the job and tell the owner.
//!   Jobs the model proposes (`schedule:create`) wait in `Proposed` until the owner
//!   approves them on the Schedule section; they never run before that.
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sail_flow::schedule::{
    Action, Creator, Deliver, Due, Job, JobState, Outcome, Run, Store, describe, next_after, parse,
    plan,
};
use serde_json::{Value, json};
use tracing::{info, warn};

/// How often the scheduler looks for due slots.
pub const TICK: Duration = Duration::from_secs(30);
/// A slot missed while SAIL was down still runs if it is at most this old.
pub const GRACE_SECS: i64 = 2 * 3600;

tokio::task_local! {
    static IN_SCHEDULED_RUN: ();
}

/// True inside a scheduled run: such a run must not create scheduled jobs.
pub fn in_scheduled_run() -> bool {
    IN_SCHEDULED_RUN.try_with(|_| ()).is_ok()
}

/// Runs one job's action and returns the text result.
#[async_trait]
pub trait RunExecutor: Send + Sync {
    async fn run(&self, job: &Job) -> Result<String, String>;
}

/// What the scheduler needs to know about the owner right now.
pub trait Presence: Send + Sync {
    fn voice_active(&self) -> bool;
    /// Quiet hours at `now`, and when they end.
    fn quiet(&self, now: DateTime<Utc>) -> (bool, Option<DateTime<Utc>>);
    /// The owner's IANA time zone (for new jobs and the status line).
    fn timezone(&self) -> String;
    /// Put a finished run in the owner's inbox. False when there is no inbox.
    fn deliver(&self, job: &Job, run: &Run) -> bool;
    /// Tell the owner a job was paused after repeated failures.
    fn notify_paused(&self, job: &Job);
}

/// The scheduler: the job store plus how to run and deliver.
pub struct ScheduleService {
    pub store: Store,
    executor: Arc<dyn RunExecutor>,
    presence: Arc<dyn Presence>,
    in_flight: Mutex<BTreeSet<String>>,
}

pub fn iso(secs: i64) -> Option<String> {
    DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// 32 lowercase hex characters.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

impl ScheduleService {
    pub fn new(dir: PathBuf, executor: Arc<dyn RunExecutor>, presence: Arc<dyn Presence>) -> Self {
        Self {
            store: Store { dir },
            executor,
            presence,
            in_flight: Mutex::new(BTreeSet::new()),
        }
    }

    /// The status strip: quiet hours, live voice, time zone.
    pub fn status(&self, now: DateTime<Utc>) -> Value {
        let (quiet, until) = self.presence.quiet(now);
        json!({
            "quiet_now": quiet,
            "quiet_until": until.map(|t| t.to_rfc3339_opts(SecondsFormat::Secs, true)),
            "voice_active": self.presence.voice_active(),
            "timezone": self.presence.timezone(),
        })
    }

    /// A job with its canonical schedule text, next run and last run.
    pub fn view(&self, job: &Job, now: i64) -> Value {
        let spec = parse(&job.when_text, &job.tz, job.anchor);
        let next = match (&spec, job.state) {
            (Ok(spec), JobState::Active | JobState::Paused | JobState::Proposed) => {
                next_after(spec, now.max(job.last_slot.unwrap_or(i64::MIN))).and_then(iso)
            }
            _ => None,
        };
        let mut out = json!(job);
        out["describe"] = json!(match &spec {
            Ok(spec) => describe(spec),
            Err(e) => format!("unreadable schedule: {e}"),
        });
        out["next_run"] = json!(next);
        out["last_run"] = json!(self.store.runs(&job.id).ok().and_then(|r| r.into_iter().next()));
        out
    }

    /// Canonical text and the next five runs for a schedule being typed.
    pub fn preview(when: &str, tz: &str, now: i64) -> Result<Value, String> {
        let spec = parse(when, tz, now)?;
        let mut next = Vec::new();
        let mut after = now;
        while next.len() < 5 {
            let Some(slot) = next_after(&spec, after) else {
                break;
            };
            next.extend(iso(slot));
            after = slot;
        }
        Ok(json!({"describe": describe(&spec), "next": next}))
    }

    /// Create a job. The owner's start Active; the model's start Proposed.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        name: &str,
        when: &str,
        tz: &str,
        action: Action,
        deliver: Deliver,
        created_by: Creator,
        reason: Option<String>,
        now: i64,
    ) -> Result<Job, String> {
        if created_by == Creator::Model && in_scheduled_run() {
            return Err("a scheduled run can't create scheduled jobs".into());
        }
        let job = Job {
            id: new_id(),
            name: name.trim().to_string(),
            when_text: when.trim().to_string(),
            tz: tz.to_string(),
            anchor: now,
            action,
            deliver,
            state: match created_by {
                Creator::Owner => JobState::Active,
                Creator::Model => JobState::Proposed,
            },
            created_by,
            created_at: now,
            last_slot: None,
            failure_streak: 0,
            reason,
        };
        self.store.create(job)
    }

    /// Owner actions on one job.
    pub fn set_state(&self, id: &str, action: &str) -> Result<Job, String> {
        let mut job = self.store.get(id)?;
        job.state = match (action, job.state) {
            ("pause", JobState::Active) => JobState::Paused,
            ("resume", JobState::Paused) => {
                job.failure_streak = 0;
                JobState::Active
            }
            ("approve", JobState::Proposed) => JobState::Active,
            (_, state) => {
                return Err(format!("can't {action} a job that is {}", state_name(state)));
            }
        };
        self.store.update(&job)?;
        Ok(job)
    }

    /// One scheduler pass. Returns how many runs it started.
    pub fn tick(self: &Arc<Self>, now: i64) -> usize {
        let at = DateTime::from_timestamp(now, 0).unwrap_or_else(Utc::now);
        if !self.presence.quiet(at).0 {
            self.deliver_held(now);
        }
        if self.presence.voice_active() {
            return 0;
        }
        let jobs = match self.store.jobs() {
            Ok(jobs) => jobs,
            Err(error) => {
                warn!(%error, "scheduled jobs unreadable; nothing runs");
                return 0;
            }
        };
        let planned = plan(&jobs, now, GRACE_SECS);
        for missed in &planned.missed {
            if let Err(error) = self.store.record_missed(missed) {
                warn!(%error, job = %missed.job_id, "missed runs not recorded");
            }
        }
        let mut started = 0;
        for due in planned.due {
            if self.start(due, now).is_ok() {
                started += 1;
            }
        }
        started
    }

    /// Run a job now (owner's "Run now"), outside its schedule.
    pub fn run_now(self: &Arc<Self>, id: &str, now: i64) -> Result<Job, String> {
        let job = self.store.get(id)?;
        if job.state == JobState::Proposed {
            return Err("approve this job before running it".into());
        }
        let due = Due {
            job_id: job.id.clone(),
            slot: now.max(job.last_slot.unwrap_or(0) + 1),
            catch_up: false,
        };
        self.start(due, now)?;
        self.store.get(id)
    }

    fn start(self: &Arc<Self>, due: Due, now: i64) -> Result<(), String> {
        let run = self.store.start(&due, now)?;
        let job = self.store.get(&due.job_id)?;
        self.in_flight.lock().unwrap().insert(run.id.clone());
        let this = self.clone();
        tokio::spawn(async move { this.execute(job, run).await });
        Ok(())
    }

    async fn execute(self: Arc<Self>, job: Job, run: Run) {
        info!(job = %job.id, run = %run.id, catch_up = run.catch_up, "Scheduled run started");
        let executor = self.executor.clone();
        let job_for_run = job.clone();
        let result = IN_SCHEDULED_RUN
            .scope((), async move { executor.run(&job_for_run).await })
            .await;
        let (outcome, summary) = match result {
            Ok(text) => (Outcome::Ok, text),
            Err(error) => (Outcome::Failed, error),
        };
        let now = Utc::now().timestamp();
        match self.store.finish_run(&run.id, outcome, &summary, now) {
            Ok((finished, notify)) => {
                info!(job = %job.id, run = %run.id, outcome = ?finished.outcome, "Scheduled run finished");
                if notify && let Ok(job) = self.store.get(&job.id) {
                    self.presence.notify_paused(&job);
                }
                let at = DateTime::from_timestamp(now, 0).unwrap_or_else(Utc::now);
                if finished.outcome == Outcome::Ok && !self.presence.quiet(at).0 {
                    self.deliver(&job, &finished, now);
                }
            }
            Err(error) => warn!(%error, run = %run.id, "scheduled run result not saved"),
        }
        self.in_flight.lock().unwrap().remove(&run.id);
    }

    fn deliver(&self, job: &Job, run: &Run, now: i64) {
        if job.deliver == Deliver::Web {
            self.presence.deliver(job, run);
        }
        if let Err(error) = self.store.mark_delivered(&run.id, now) {
            warn!(%error, run = %run.id, "delivery not recorded");
        }
    }

    /// Deliver results that were held for quiet hours.
    fn deliver_held(&self, now: i64) {
        let Ok(jobs) = self.store.jobs() else {
            return;
        };
        for job in jobs.iter().filter(|j| j.deliver == Deliver::Web) {
            for run in self.store.runs(&job.id).unwrap_or_default() {
                if run.outcome == Outcome::Ok && run.delivered_at.is_none() {
                    self.deliver(job, &run, now);
                }
            }
        }
    }

    /// True while any run is executing.
    pub fn busy(&self) -> bool {
        !self.in_flight.lock().unwrap().is_empty()
    }
}

fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Active => "active",
        JobState::Paused => "paused",
        JobState::Proposed => "waiting for your approval",
        JobState::Done => "done",
    }
}

static GLOBAL: OnceLock<Arc<ScheduleService>> = OnceLock::new();

/// The daemon's scheduler, once started.
pub fn global() -> Option<Arc<ScheduleService>> {
    GLOBAL.get().cloned()
}

/// Start the scheduler loop (once per process): recover interrupted runs, then tick.
pub fn start(service: Arc<ScheduleService>) -> Arc<ScheduleService> {
    let service = GLOBAL.get_or_init(|| service).clone();
    match service.store.recover(Utc::now().timestamp()) {
        Ok(0) => {}
        Ok(n) => warn!(runs = n, "scheduled runs interrupted by a restart were marked failed"),
        Err(error) => warn!(%error, "scheduled runs not recovered"),
    }
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let looping = service.clone();
        handle.spawn(async move {
            let mut timer = tokio::time::interval(TICK);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                timer.tick().await;
                looping.tick(Utc::now().timestamp());
            }
        });
    }
    service
}

/// The text a model-run job is given: the owner's request plus the run's rules.
pub fn run_prompt(job: &Job) -> String {
    let task = match &job.action {
        Action::Prompt { text } => text.clone(),
        Action::Skill { name, input } => format!(
            "Use the skill \"{name}\": load it with skill_view first and follow it.\nInput: {input}"
        ),
        Action::Workflow { id } => format!("Run workflow {id}."),
    };
    format!(
        "This is a scheduled run of \"{}\" ({}). Nobody is watching: actions that need the \
owner's approval will not run, so say which ones you could not do. Reply with the result \
only.\n\n{task}",
        job.name, job.when_text
    )
}

/// The daemon's [`Presence`]: Pulse settings, the resource scheduler, the attention inbox.
pub struct DaemonPresence {
    pub pulse: Option<Arc<sail_pulse::Pulse>>,
}

impl Presence for DaemonPresence {
    fn voice_active(&self) -> bool {
        sail_common::resources::global().voice_active()
    }

    fn quiet(&self, now: DateTime<Utc>) -> (bool, Option<DateTime<Utc>>) {
        let Some(settings) = self.pulse.as_ref().and_then(|p| p.settings().ok()) else {
            return (false, None);
        };
        if !settings.is_quiet_at(now) {
            return (false, None);
        }
        let minute = i64::from(settings.local_minute(now));
        let end = i64::from(settings.quiet_end_minute);
        let wait = (end - minute).rem_euclid(24 * 60);
        let until = now + chrono::Duration::minutes(wait) - chrono::Duration::seconds(i64::from(
            chrono::Timelike::second(&now),
        ));
        (true, Some(until))
    }

    fn timezone(&self) -> String {
        self.pulse
            .as_ref()
            .and_then(|p| p.settings().ok())
            .map(|s| s.timezone)
            .unwrap_or_else(|| "UTC".into())
    }

    fn deliver(&self, job: &Job, run: &Run) -> bool {
        signal(
            "scheduled_run",
            &format!("schedule:{}", run.id),
            &job.name,
            &run.summary,
        )
    }

    fn notify_paused(&self, job: &Job) {
        signal(
            "scheduled_job_paused",
            &format!("schedule:{}:paused:{}", job.id, job.last_slot.unwrap_or(0)),
            &format!("Paused \"{}\"", job.name),
            "It failed 3 times in a row. Open Schedule to see why and resume it.",
        );
    }
}

fn signal(kind: &str, key: &str, title: &str, body: &str) -> bool {
    let Some(proactive) = crate::proactive::global() else {
        return false;
    };
    let attention = proactive.initiative.attention();
    let now = Utc::now();
    let settings = attention.settings().unwrap_or_default();
    let context = proactive.context(now, &settings);
    let body: String = body.chars().take(1200).collect();
    let signal = sail_pulse::attention::Signal {
        source: sail_pulse::attention::Source::Reminders,
        kind: kind.into(),
        dedupe_key: key.into(),
        title: title.chars().take(120).collect(),
        body,
        urgency: 0.5,
        relevance: 0.9,
        novelty: 0.8,
        confidence: 0.9,
        deadline: None,
        provenance: sail_pulse::attention::Provenance::UserCommitment,
        link: Some("/#agents?section=schedule".into()),
        observed_at: now,
    };
    match attention.ingest(signal, context) {
        Ok(_) => true,
        Err(error) => {
            warn!(%error, "scheduled result not added to the inbox");
            false
        }
    }
}
