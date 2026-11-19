//! Proactive wiring: runs Discovery's initiative loop on a timer, feeds it observations
//! from stores Dialogue already holds, and hands finished escalations back.
//!
//! Every tick is deterministic and model-free. It reads the local Moodle store, the data
//! disk and finished escalation jobs, then lets Pulse's attention store decide what (if
//! anything) to surface. Nothing here calls a model or the network.
//!
//! Set `SAIL_PROACTIVE=0` to turn the loop off; `SAIL_PROACTIVE_TICK_SECS` (60–3600,
//! default 300) sets the period.
//!
//! The one exception to "no model calls" is the Curiosity Engine (#453): after a tick, when
//! [`sail_discovery::curiosity::admit`] allows it (Balanced or Proactive autonomy, owner
//! idle, no voice, quiet hours or focus, daily allowance left), one bounded local-model
//! investigation runs in the background. `SAIL_CURIOSITY=0` turns it off.
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, Utc};
use sail_continuity::{Continuity, TaskFilter, TaskState};
use sail_discovery::agenda::AgendaStore;
use sail_discovery::curiosity::{
    CuriosityLedger, CuriosityLimits, Gate, Investigator, Skip, admit, run_once,
};
use sail_discovery::detect::{ForgeJobState, Level, Observation, SchoolChangeKind, SchoolItemKind};
use sail_discovery::github::{GhCliActivity, GithubActivity, GithubError, GithubPoller};
use sail_discovery::initiative::{Initiative, TickReport};
use sail_discovery::tasks::{TaskEnd, TaskOutcome, task_signals};
use sail_mind::escalation::service::{self as escalation, EscalationService};
use sail_mind::escalation::{JobState, Origin};
use sail_pulse::Pulse;
use sail_pulse::attention::store::AttentionStore;
use sail_pulse::attention::{AttentionContext, AttentionSettings, DigestBatch};
use tracing::{info, warn};

pub struct Proactive {
    pub initiative: Arc<Initiative>,
    pub escalation: Option<Arc<EscalationService>>,
    pulse: Option<Arc<Pulse>>,
    last_tick: Mutex<TickReport>,
    last_digest: Mutex<Option<DigestBatch>>,
    last_check: Mutex<DateTime<Utc>>,
    answered: Mutex<Vec<Observation>>,
    curiosity: Mutex<Option<Arc<Curiosity>>>,
    /// GitHub activity source and its pacing; set only by [`start`], so tests never run `gh`.
    github: Mutex<Option<GithubWatch>>,
    /// When Capital suggestions were last read (at most hourly).
    last_capital: Mutex<Option<DateTime<Utc>>>,
}

struct GithubWatch {
    source: Arc<dyn GithubActivity>,
    poller: GithubPoller,
    unavailable_logged: bool,
}

/// Capital's suggestions are recomputed from prices; reading them hourly is plenty.
const CAPITAL_EVERY: ChronoDuration = ChronoDuration::hours(1);

/// The Curiosity Engine's runtime state (#453).
pub struct Curiosity {
    pub ledger: CuriosityLedger,
    pub limits: CuriosityLimits,
    investigator: Arc<dyn Investigator>,
    running: AtomicBool,
    last_skip: Mutex<Option<Skip>>,
}

impl Curiosity {
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn last_skip(&self) -> Option<Skip> {
        *lock(&self.last_skip)
    }
}

/// Unix millis of the last finished conversation turn (0 = none yet).
static LAST_TURN_MS: AtomicI64 = AtomicI64::new(0);

/// A conversation turn just finished: background curiosity waits for the owner to be idle.
pub fn note_conversation_turn() {
    LAST_TURN_MS.store(Utc::now().timestamp_millis(), Ordering::SeqCst);
}

fn idle_for(now: DateTime<Utc>) -> Option<ChronoDuration> {
    match LAST_TURN_MS.load(Ordering::SeqCst) {
        0 => None,
        ms => Some(now - DateTime::from_timestamp_millis(ms).unwrap_or(now)),
    }
}

static GLOBAL: OnceLock<Arc<Proactive>> = OnceLock::new();

pub fn global() -> Option<Arc<Proactive>> {
    GLOBAL.get().cloned()
}

fn enabled() -> bool {
    !std::env::var("SAIL_PROACTIVE").is_ok_and(|v| v == "0" || v == "false")
}

fn tick_period() -> Duration {
    let secs = std::env::var("SAIL_PROACTIVE_TICK_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| (60..=3600).contains(s))
        .unwrap_or(300);
    Duration::from_secs(secs)
}

/// Open the stores and start the loop once per process. Returns `None` when turned off or
/// when a store can't be opened (logged; the rest of SAIL keeps working).
pub fn start(pulse: Option<Arc<Pulse>>) -> Option<Arc<Proactive>> {
    if let Some(existing) = global() {
        return Some(existing);
    }
    if !enabled() {
        info!("Proactive initiative is off (SAIL_PROACTIVE=0)");
        return None;
    }
    let attention_path = sail_common::storage::data_path("pulse").join("attention.db");
    let attention = match AttentionStore::open(&attention_path) {
        Ok(store) => Arc::new(store),
        Err(error) => {
            warn!(%error, "attention store unavailable; proactive initiative off");
            return None;
        }
    };
    let agenda = match AgendaStore::open(sail_discovery::default_agenda_path()) {
        Ok(store) => Arc::new(store),
        Err(error) => {
            warn!(%error, "agenda store unavailable; proactive initiative off");
            return None;
        }
    };
    let initiative = Arc::new(Initiative::new(attention, agenda));
    let _ = sail_discovery::install_global(initiative.clone());
    let escalation = start_escalation();
    let proactive = Proactive::new(initiative, escalation, pulse);
    proactive.watch_github(Arc::new(GhCliActivity::from_env()));
    let proactive = GLOBAL.get_or_init(|| proactive).clone();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let looping = proactive.clone();
        handle.spawn(async move { looping.run().await });
    }
    Some(proactive)
}

fn start_escalation() -> Option<Arc<EscalationService>> {
    if let Some(existing) = escalation::global() {
        return Some(existing);
    }
    let policy = match escalation::load_policy(&escalation::default_policy_path()) {
        Ok(policy) => policy,
        Err(error) => {
            warn!(%error, "escalation policy is malformed; escalation stays off");
            return None;
        }
    };
    let store =
        match sail_mind::escalation::store::EscalationStore::open(escalation::default_store_path())
        {
            Ok(store) => Arc::new(store),
            Err(error) => {
                warn!(%error, "escalation ledger unavailable; escalation stays off");
                return None;
            }
        };
    let enabled = policy.enabled;
    let service = EscalationService::new(policy, store);
    let _ = escalation::install_global(service.clone());
    if enabled {
        match service.resume() {
            Ok(0) => {}
            Ok(n) => info!(jobs = n, "resumed pending escalations"),
            Err(error) => warn!(%error, "pending escalations not resumed"),
        }
    }
    info!(enabled, "reasoning escalation ready");
    Some(service)
}

impl Proactive {
    /// An engine over the given stores, without a timer (`start` adds the timer).
    pub fn new(
        initiative: Arc<Initiative>,
        escalation: Option<Arc<EscalationService>>,
        pulse: Option<Arc<Pulse>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            initiative,
            escalation,
            pulse,
            last_tick: Mutex::new(TickReport::default()),
            last_digest: Mutex::new(None),
            last_check: Mutex::new(Utc::now()),
            answered: Mutex::new(Vec::new()),
            curiosity: Mutex::new(None),
            github: Mutex::new(None),
            last_capital: Mutex::new(None),
        })
    }

    /// Watch GitHub through `source` (production: the signed-in `gh`). Off by default in
    /// tests; `SAIL_GITHUB_DETECTOR=0` turns it off.
    pub fn watch_github(&self, source: Arc<dyn GithubActivity>) {
        if std::env::var("SAIL_GITHUB_DETECTOR").is_ok_and(|v| v == "0" || v == "false") {
            return;
        }
        *lock(&self.github) = Some(GithubWatch {
            source,
            poller: GithubPoller::default(),
            unavailable_logged: false,
        });
    }

    /// GitHub events, when the poller says it's time (at most every 15 min). Blocking.
    fn github_observations(&self, now: DateTime<Utc>) -> Vec<Observation> {
        let mut guard = lock(&self.github);
        let Some(watch) = guard.as_mut() else {
            return Vec::new();
        };
        if !watch.poller.due(now) {
            return Vec::new();
        }
        let since = watch.poller.since(now);
        match watch.source.fetch(since) {
            Ok(events) => {
                watch.poller.record(now, true);
                watch.unavailable_logged = false;
                events.into_iter().map(Observation::GitHub).collect()
            }
            Err(GithubError::Unavailable(reason)) => {
                watch.poller.record(now, false);
                if !watch.unavailable_logged {
                    info!(%reason, "GitHub detector waiting for a signed-in gh");
                    watch.unavailable_logged = true;
                }
                Vec::new()
            }
        }
    }

    /// Active High and Medium Capital suggestions, at most hourly. Blocking.
    fn capital_observations(&self, now: DateTime<Utc>) -> Vec<Observation> {
        {
            let mut last = lock(&self.last_capital);
            if last.is_some_and(|at| now - at < CAPITAL_EVERY) {
                return Vec::new();
            }
            *last = Some(now);
        }
        let Some(capital) = sail_capital::service::global() else {
            return Vec::new();
        };
        let today = capital
            .resolve_account(None)
            .and_then(|account| capital.today(&account, now));
        let today = match today {
            Ok(today) => today,
            Err(error) => {
                tracing::debug!(%error, "capital suggestions unavailable");
                return Vec::new();
            }
        };
        today
            .suggestions
            .into_iter()
            .filter_map(|s| {
                let severity = match s.severity {
                    sail_capital::suggest::Severity::High => Level::High,
                    sail_capital::suggest::Severity::Medium => Level::Medium,
                    sail_capital::suggest::Severity::Low => return None,
                };
                Some(Observation::CapitalSuggestion {
                    id: s.id,
                    severity,
                    title: s.title,
                    why: s.why,
                })
            })
            .collect()
    }

    /// Turn on idle-time investigations with `investigator` (off when `SAIL_CURIOSITY=0`).
    pub fn enable_curiosity(
        &self,
        investigator: Arc<dyn Investigator>,
        ledger: CuriosityLedger,
    ) -> bool {
        if std::env::var("SAIL_CURIOSITY").is_ok_and(|v| v == "0" || v == "false") {
            info!("Curiosity Engine is off (SAIL_CURIOSITY=0)");
            return false;
        }
        *lock(&self.curiosity) = Some(Arc::new(Curiosity {
            ledger,
            limits: CuriosityLimits::default(),
            investigator,
            running: AtomicBool::new(false),
            last_skip: Mutex::new(None),
        }));
        true
    }

    pub fn curiosity(&self) -> Option<Arc<Curiosity>> {
        lock(&self.curiosity).clone()
    }

    /// The facts the Curiosity gate decides on, now.
    pub fn curiosity_gate(&self, curiosity: &Curiosity, now: DateTime<Utc>) -> Gate {
        let settings = self.initiative.attention().settings().unwrap_or_default();
        let context = self.context(now, &settings);
        Gate {
            autonomy: settings.autonomy,
            voice_active: context.voice_active,
            quiet_hours: context.quiet_hours,
            focus: context.focus_until.is_some(),
            idle_for: idle_for(now),
            runs_today: curiosity.ledger.runs_today(now).unwrap_or(u32::MAX),
            running: curiosity.running(),
        }
    }

    /// Start one investigation in the background when the gate allows it. Returns the
    /// reason when it doesn't.
    pub fn maybe_investigate(self: &Arc<Self>) -> Result<(), Skip> {
        let Some(curiosity) = self.curiosity() else {
            return Err(Skip::Autonomy);
        };
        let gate = self.curiosity_gate(&curiosity, Utc::now());
        if let Err(skip) = admit(&gate, &curiosity.limits) {
            *lock(&curiosity.last_skip) = Some(skip);
            return Err(skip);
        }
        if curiosity.running.swap(true, Ordering::SeqCst) {
            return Err(Skip::Running);
        }
        let this = self.clone();
        let autonomy = gate.autonomy;
        tokio::spawn(async move {
            let cancel = tokio_util::sync::CancellationToken::new();
            // Live voice always wins: cancel the moment a voice turn starts.
            let watch = {
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    loop {
                        if sail_common::resources::global().voice_active() {
                            cancel.cancel();
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                })
            };
            let result = run_once(
                this.initiative.agenda(),
                &curiosity.ledger,
                curiosity.investigator.as_ref(),
                autonomy,
                &curiosity.limits,
                cancel,
            )
            .await;
            watch.abort();
            match result {
                Ok(report) => {
                    info!(
                        item = %report.receipt.item_id,
                        outcome = ?report.receipt.outcome,
                        tokens = report.receipt.tokens_used,
                        steps = report.receipt.tool_steps,
                        "curiosity run finished"
                    );
                    if let Some(observation) = report.observation {
                        lock(&this.answered).push(observation);
                    }
                    *lock(&curiosity.last_skip) = None;
                }
                Err(skip) => *lock(&curiosity.last_skip) = Some(skip),
            }
            curiosity.running.store(false, Ordering::SeqCst);
        });
        Ok(())
    }

    async fn run(self: Arc<Self>) {
        let mut answers = self.escalation.as_ref().map(|s| s.subscribe());
        let mut timer = tokio::time::interval(tick_period());
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = timer.tick() => {}
                job = async {
                    match answers.as_mut() {
                        Some(rx) => rx.recv().await.ok(),
                        None => std::future::pending().await,
                    }
                } => {
                    // Typed turns that waited already showed the answer; only answers that
                    // arrived after their turn ended, or background ones, are surfaced.
                    if let Some(job) = job
                        && job.state == JobState::Done
                        && job.origin != Origin::Owner
                    {
                        lock(&self.answered).push(Observation::EscalationAnswered {
                            job_id: job.id,
                            model: job.model,
                            question: job.question,
                            correlation_id: job.correlation_id,
                        });
                    }
                    continue;
                }
            }
            let this = self.clone();
            let _ = tokio::task::spawn_blocking(move || this.tick_now()).await;
            let _ = self.maybe_investigate();
        }
    }

    /// One tick: gather observations, run the initiative, release a due digest.
    pub fn tick_now(&self) -> TickReport {
        let now = Utc::now();
        let since = *lock(&self.last_check);
        let mut observations = std::mem::take(&mut *lock(&self.answered));
        let offset = self
            .pulse
            .as_ref()
            .and_then(|p| p.settings().ok())
            .map_or(0, |s| s.utc_offset_minutes);
        observations.extend(moodle_observations(now, offset));
        observations.extend(forge_observations());
        observations.extend(self.capital_observations(now));
        observations.extend(self.github_observations(now));
        if let Some(disk) = disk_observation() {
            observations.push(disk);
        }
        let settings = self.initiative.attention().settings().unwrap_or_default();
        let context = self.context(now, &settings);
        let report = self.initiative.tick(&observations, context.clone());
        for signal in task_signals(&task_outcomes(since, now), since, now) {
            if let Err(error) = self.initiative.attention().ingest(signal, context.clone()) {
                warn!(%error, "task outcome not ingested");
            }
        }
        if let Some(signal) = crate::nightshift::global().and_then(|service| {
            let store = service.store();
            let today = now
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string();
            store
                .settings()
                .morning_greeting
                .then(|| store.morning(&today))
                .flatten()
                .and_then(|report| nightshift_signal(&report, now))
        }) && let Err(error) = self.initiative.attention().ingest(signal, context.clone())
        {
            warn!(%error, "Night Shift morning report not ingested");
        }
        crate::escalation_outcomes::settle_forge_jobs();
        *lock(&self.last_tick) = report.clone();
        let previous = std::mem::replace(&mut *lock(&self.last_check), now);
        if digest_due(&settings, offset, previous, now) {
            match self.initiative.attention().release_digest(now) {
                Ok(Some(batch)) => {
                    info!(items = batch.items.len(), "attention digest released");
                    *lock(&self.last_digest) = Some(batch);
                }
                Ok(None) => {}
                Err(error) => warn!(%error, "attention digest not released"),
            }
        }
        if let Some(escalation) = &self.escalation {
            let _ = escalation.store().prune(90, now);
        }
        let _ = self.initiative.attention().prune(30, now);
        report
    }

    pub fn context(&self, now: DateTime<Utc>, settings: &AttentionSettings) -> AttentionContext {
        let quiet = self
            .pulse
            .as_ref()
            .and_then(|p| p.settings().ok())
            .is_some_and(|s| s.is_quiet_at(now));
        AttentionContext {
            now,
            quiet_hours: quiet,
            focus_until: settings.focus_until.filter(|until| *until > now),
            voice_active: sail_common::resources::global().voice_active(),
            busy: false,
            interrupts_last_hour: 0,
            suggestions_today: 0,
        }
    }

    pub fn last_tick(&self) -> TickReport {
        lock(&self.last_tick).clone()
    }

    pub fn last_digest(&self) -> Option<DigestBatch> {
        lock(&self.last_digest).clone()
    }

    pub fn set_last_digest(&self, batch: Option<DigestBatch>) {
        *lock(&self.last_digest) = batch;
    }
}

/// The "could raise" brief for a conversation prompt. Only at the start of a conversation
/// (the first two turns) so it doesn't grow every prompt.
pub fn conversation_brief(hot_turns: usize) -> Option<String> {
    if hot_turns >= 2 {
        return None;
    }
    let brief = global()?.initiative.conversation_brief(Utc::now());
    (!brief.is_empty()).then_some(brief)
}

/// A digest time `m` (local minute) passed in `(previous, now]`.
pub fn digest_due(
    settings: &AttentionSettings,
    offset_minutes: i32,
    previous: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    if now <= previous {
        return false;
    }
    let offset = ChronoDuration::minutes(i64::from(offset_minutes));
    let local_now = now + offset;
    let local_prev = previous + offset;
    settings.digest_minutes.iter().any(|&m| {
        let Some(at) = NaiveTime::from_hms_opt(u32::from(m / 60) % 24, u32::from(m % 60), 0) else {
            return false;
        };
        // Check today's and yesterday's occurrence (a tick can straddle midnight).
        [
            local_now.date_naive(),
            local_now.date_naive() - ChronoDuration::days(1),
        ]
        .into_iter()
        .any(|day| {
            let at = day.and_time(at).and_utc();
            at > local_prev && at <= local_now
        })
    })
}

/// Forge jobs in a state the owner may care about. Dedupe keys include the state, so each
/// state is news once.
fn forge_observations() -> Vec<Observation> {
    use sail_forge::ForgeTaskState as S;
    let Ok(orchestrator) = sail_forge::global_orchestrator() else {
        return Vec::new();
    };
    // Only recent activity is news; old finished or abandoned jobs stay quiet.
    let recent = Utc::now() - ChronoDuration::days(2);
    orchestrator
        .list_all_jobs()
        .into_iter()
        .filter(|job| job.events.iter().any(|e| e.timestamp >= recent))
        .filter_map(|job| {
            let state = match job.state {
                S::AwaitingUser => ForgeJobState::AwaitingUser,
                S::ReviewPending => ForgeJobState::ReviewPending,
                S::Blocked => ForgeJobState::Blocked,
                S::Completed => ForgeJobState::Completed,
                _ => return None,
            };
            Some(Observation::ForgeJob {
                subject: format!("{}#{}", job.repo, job.issue_number),
                job_id: job.job_id,
                state,
            })
        })
        .collect()
}

/// Continuity stores whose finished work is announced. Forge jobs are not taken from
/// here: `forge_observations` already reports them with richer states.
static TASK_STORES: Mutex<Vec<Arc<Continuity>>> = Mutex::new(Vec::new());

/// Most recent tasks read per store and tick (fixed a priori: far more than finish in one
/// five-minute tick).
const TASK_SCAN_LIMIT: usize = 100;

/// Announce outcomes of tasks in `store` (idempotent per store).
pub fn watch_tasks(store: Arc<Continuity>) {
    let mut stores = lock(&TASK_STORES);
    if !stores.iter().any(|s| Arc::ptr_eq(s, &store)) {
        stores.push(store);
    }
}

/// The owner as the owner reads it. Unknown owners keep their own name.
fn task_owner_label(owner: &str) -> Option<String> {
    match owner {
        "sail-forge" | "forge" => None,
        "sail-arena-learn" => Some("Course Factory".into()),
        "sail-agents" | "agents" | "Agents" => Some("Plan".into()),
        other => Some(other.to_owned()),
    }
}

/// Tasks that reached an end state the owner cares about in `(since, now]`.
fn task_outcomes(since: DateTime<Utc>, now: DateTime<Utc>) -> Vec<TaskOutcome> {
    let stores = lock(&TASK_STORES).clone();
    let filter = TaskFilter {
        owner: None,
        include_terminal: true,
        limit: Some(TASK_SCAN_LIMIT),
    };
    let mut outcomes = Vec::new();
    for store in stores {
        let tasks = match store.list(&filter) {
            Ok(tasks) => tasks,
            Err(error) => {
                warn!(%error, "task outcomes unavailable");
                continue;
            }
        };
        for task in tasks {
            let end = match task.state {
                TaskState::Succeeded => TaskEnd::Succeeded,
                TaskState::Failed => TaskEnd::Failed,
                TaskState::NeedsIntervention => TaskEnd::NeedsIntervention,
                _ => continue,
            };
            if task.updated_at <= since || task.updated_at > now {
                continue;
            }
            let Some(owner) = task_owner_label(&task.owner) else {
                continue;
            };
            outcomes.push(TaskOutcome {
                id: task.id,
                title: task.title,
                owner,
                end,
                ended_at: task.updated_at,
                summary: task.reason.or(task.stage).unwrap_or_default(),
            });
        }
    }
    outcomes
}

/// A readable course name: Moodle's full name, falling back to the short code.
fn course_label(short_name: &str, full_name: &str) -> String {
    let full = full_name.trim();
    if full.is_empty() {
        short_name.trim().to_owned()
    } else {
        full.to_owned()
    }
}

fn moodle_observations(now: DateTime<Utc>, offset_minutes: i32) -> Vec<Observation> {
    use sail_moodle::{ChangeKind, Confidence, ItemKind};
    let Some(service) = sail_moodle::service::global() else {
        return Vec::new();
    };
    let store = service.store();
    let names: std::collections::HashMap<i64, String> = store
        .courses()
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c.id, course_label(&c.short_name, &c.full_name)))
        .collect();
    let course = |id: i64| {
        names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("course {id}"))
    };
    let offset = ChronoDuration::minutes(i64::from(offset_minutes));
    let today = (now + offset).date_naive();
    let mut out = Vec::new();
    for item in store.items(None, Some(today), false).unwrap_or_default() {
        let kind = match item.kind {
            ItemKind::Exam => SchoolItemKind::Exam,
            ItemKind::Test => SchoolItemKind::Test,
            ItemKind::Quiz => SchoolItemKind::Quiz,
            ItemKind::Deadline => SchoolItemKind::Deadline,
            ItemKind::Presentation => SchoolItemKind::Presentation,
            ItemKind::Lab => SchoolItemKind::Lab,
            ItemKind::ScheduleChange => SchoolItemKind::ScheduleChange,
            ItemKind::Registration => SchoolItemKind::Registration,
            ItemKind::Other => SchoolItemKind::Other,
        };
        // Without a time, deliveries are due at the end of the day and sittings in the
        // morning, so the reminder is never later than the real moment.
        let time = item.when.time.unwrap_or_else(|| match kind {
            SchoolItemKind::Deadline | SchoolItemKind::Registration => {
                NaiveTime::from_hms_opt(23, 59, 0).expect("time")
            }
            _ => NaiveTime::from_hms_opt(9, 0, 0).expect("time"),
        });
        let due = item.when.date.and_time(time).and_utc() - offset;
        out.push(Observation::SchoolItem {
            id: item.id.clone(),
            course: course(item.course_id),
            kind,
            title: item.title.clone(),
            due,
            weight_percent: item.weight_percent,
            confidence: match item.confidence {
                Confidence::Confirmed => 0.95,
                Confidence::Likely => 0.75,
                Confidence::Uncertain => 0.4,
            },
        });
    }
    let since = now - ChronoDuration::days(7);
    for stored in store
        .changes(Some(since), None, false, 100)
        .unwrap_or_default()
    {
        let change = stored.change;
        let kind = match change.kind {
            ChangeKind::GradeReleased => SchoolChangeKind::GradeReleased,
            ChangeKind::AnnouncementPosted => SchoolChangeKind::AnnouncementPosted,
            ChangeKind::AssignmentDueChanged | ChangeKind::EventMoved => {
                SchoolChangeKind::DeadlineMoved
            }
            ChangeKind::AssignmentAdded => SchoolChangeKind::AssignmentAdded,
            ChangeKind::EventAdded => SchoolChangeKind::EventAdded,
            ChangeKind::FileAdded => SchoolChangeKind::FileAdded,
            _ => continue,
        };
        out.push(Observation::SchoolChange {
            id: change.id,
            course: course(change.course_id),
            kind,
            title: change.title,
            observed_at: change.observed_at,
        });
    }
    out
}

#[cfg(unix)]
fn disk_observation() -> Option<Observation> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let root = sail_common::storage::resolve().path;
    let c = CString::new(root.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a properly sized out-parameter.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 || s.f_blocks == 0 {
        return None;
    }
    let free = s.f_bavail as f64 / s.f_blocks as f64 * 100.0;
    Some(Observation::DiskSpace {
        mount: root.display().to_string(),
        free_percent: free as f32,
    })
}

#[cfg(not(unix))]
fn disk_observation() -> Option<Observation> {
    None
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn task_outcomes_report_ended_work_once_with_readable_owners_and_skip_forge() {
        use sail_continuity::{Outcome, ResumePolicy, TaskSpec};
        let store = Arc::new(Continuity::open_in_memory().expect("store"));
        watch_tasks(store.clone());
        watch_tasks(store.clone()); // idempotent
        let since = Utc::now() - ChronoDuration::minutes(1);
        let submit = |id: &str, owner: &str| {
            store
                .submit(TaskSpec {
                    id: Some(id.into()),
                    owner: owner.into(),
                    kind: "test".into(),
                    title: format!("{id} title"),
                    dedupe_key: None,
                    resume: ResumePolicy::Manual,
                    checkpoint: serde_json::json!({}),
                })
                .expect("submit")
        };
        for (id, owner, outcome) in [
            (
                "r26-factory",
                "sail-arena-learn",
                Some(Outcome::Failed("2 sources unreadable".into())),
            ),
            ("r26-forge", "sail-forge", Some(Outcome::Succeeded)),
            ("r26-plan", "sail-agents", Some(Outcome::Succeeded)),
            (
                "r26-cancel",
                "sail-agents",
                Some(Outcome::Cancelled("owner".into())),
            ),
            ("r26-running", "sail-agents", None),
        ] {
            submit(id, owner);
            let lease = store.claim(id).expect("claim");
            if let Some(outcome) = outcome {
                store.finish(&lease, outcome).expect("finish");
            }
        }
        let now = Utc::now() + ChronoDuration::minutes(1);
        let mut mine: Vec<(String, String, TaskEnd)> = task_outcomes(since, now)
            .into_iter()
            .filter(|o| o.id.starts_with("r26-"))
            .map(|o| (o.id, o.owner, o.end))
            .collect();
        mine.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            mine,
            vec![
                (
                    "r26-factory".into(),
                    "Course Factory".into(),
                    TaskEnd::Failed
                ),
                ("r26-plan".into(), "Plan".into(), TaskEnd::Succeeded),
            ]
        );
        let failed = task_outcomes(since, now)
            .into_iter()
            .find(|o| o.id == "r26-factory")
            .expect("factory outcome");
        assert_eq!(failed.summary, "2 sources unreadable");
        // Nothing new after the window moves past them.
        assert!(
            task_outcomes(now, now + ChronoDuration::minutes(1))
                .iter()
                .all(|o| !o.id.starts_with("r26-"))
        );
    }

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 12, h, m, 0).unwrap()
    }

    #[test]
    fn digest_fires_once_when_a_digest_time_passes() {
        let settings = AttentionSettings::default(); // 08:00 and 18:00 local
        assert!(digest_due(&settings, 0, at(7, 58), at(8, 3)));
        assert!(!digest_due(&settings, 0, at(8, 3), at(8, 8)));
        assert!(!digest_due(&settings, 0, at(9, 0), at(9, 0)));
        // UTC+1: 08:00 local is 07:00 UTC.
        assert!(digest_due(&settings, 60, at(6, 58), at(7, 2)));
        assert!(!digest_due(&settings, 60, at(7, 58), at(8, 3)));
    }

    #[test]
    fn digest_across_midnight_and_long_gaps() {
        let settings = AttentionSettings {
            digest_minutes: vec![0],
            ..AttentionSettings::default()
        };
        let before = Utc.with_ymd_and_hms(2026, 11, 11, 23, 58, 0).unwrap();
        assert!(digest_due(&settings, 0, before, at(0, 3)));
        // A daemon that slept through 18:00 releases on wake.
        assert!(digest_due(
            &AttentionSettings::default(),
            0,
            at(17, 0),
            at(19, 0)
        ));
    }
}

/// Pure. The morning report as one attention signal: the spoken template is the title (so the
/// arrival greeting can say it), one line per item is the body. One signal per date (the
/// dedupe key), none when nothing ran.
pub(crate) fn nightshift_signal(
    report: &sail_forge::nightshift::MorningReport,
    now: DateTime<Utc>,
) -> Option<sail_pulse::attention::Signal> {
    use sail_pulse::attention::{Provenance, Signal, Source};
    if report.spoken.trim().is_empty() {
        return None;
    }
    let body = report
        .items
        .iter()
        .map(|item| format!("{}: {}", item.title, item.line))
        .collect::<Vec<_>>()
        .join("\n");
    Some(Signal {
        source: Source::Projects,
        kind: "night_shift_morning".into(),
        dedupe_key: format!("nightshift:morning:{}", report.date),
        title: report.spoken.clone(),
        body: body.chars().take(600).collect(),
        // Owner-requested work finished overnight: worth saying on arrival, never urgent.
        urgency: if report.needs_you > 0 { 0.7 } else { 0.5 },
        relevance: 0.9,
        novelty: 1.0,
        confidence: 1.0,
        deadline: None,
        provenance: Provenance::Observation,
        link: None,
        observed_at: now,
    })
}

#[cfg(test)]
mod nightshift_signal_tests {
    use super::*;
    use sail_forge::nightshift::{ErrandId, MorningItem, MorningOutcome, MorningReport, NightItem};

    fn report(spoken: &str, needs_you: u32) -> MorningReport {
        MorningReport {
            date: "2026-11-19".into(),
            ready: 1,
            needs_you,
            failed: 0,
            not_reached: 0,
            items: vec![MorningItem {
                item: NightItem::Errand {
                    id: ErrandId::parse("e-0a1b2c3d4e5f").unwrap(),
                },
                title: "Show the next dividend date".into(),
                outcome: MorningOutcome::Ready,
                line: "Changed 2 files (+40 −3). 41 tests passed.".into(),
            }],
            spoken: spoken.into(),
            halted: None,
        }
    }

    #[test]
    fn the_morning_report_becomes_one_dated_signal() {
        let now = Utc::now();
        let signal =
            nightshift_signal(&report("Overnight I finished 1 of 1 jobs.", 0), now).unwrap();
        assert_eq!(signal.dedupe_key, "nightshift:morning:2026-11-19");
        assert_eq!(signal.title, "Overnight I finished 1 of 1 jobs.");
        assert!(
            signal
                .body
                .contains("Show the next dividend date: Changed 2 files")
        );
        assert_eq!(signal.urgency, 0.5);
        assert_eq!(
            nightshift_signal(&report("x", 1), now).unwrap().urgency,
            0.7
        );
        assert_eq!(
            nightshift_signal(&report("x", 0), now),
            nightshift_signal(&report("x", 0), now)
        );
    }

    #[test]
    fn nothing_ran_means_no_signal() {
        assert!(nightshift_signal(&report("", 0), Utc::now()).is_none());
        assert!(nightshift_signal(&report("   ", 0), Utc::now()).is_none());
    }
}
