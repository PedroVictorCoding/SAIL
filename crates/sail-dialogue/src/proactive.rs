//! Proactive wiring: runs Discovery's initiative loop on a timer, feeds it observations
//! from stores Dialogue already holds, and hands finished escalations back.
//!
//! Every tick is deterministic and model-free. It reads the local Moodle store, the data
//! disk and finished escalation jobs, then lets Pulse's attention store decide what (if
//! anything) to surface. Nothing here calls a model or the network.
//!
//! Set `SAIL_PROACTIVE=0` to turn the loop off; `SAIL_PROACTIVE_TICK_SECS` (60–3600,
//! default 300) sets the period.
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, Utc};
use sail_discovery::agenda::AgendaStore;
use sail_discovery::detect::{Observation, SchoolChangeKind, SchoolItemKind};
use sail_discovery::initiative::{Initiative, TickReport};
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
        })
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
        }
    }

    /// One tick: gather observations, run the initiative, release a due digest.
    pub fn tick_now(&self) -> TickReport {
        let now = Utc::now();
        let mut observations = std::mem::take(&mut *lock(&self.answered));
        let offset = self
            .pulse
            .as_ref()
            .and_then(|p| p.settings().ok())
            .map_or(0, |s| s.utc_offset_minutes);
        observations.extend(moodle_observations(now, offset));
        if let Some(disk) = disk_observation() {
            observations.push(disk);
        }
        let settings = self.initiative.attention().settings().unwrap_or_default();
        let context = self.context(now, &settings);
        let report = self.initiative.tick(&observations, context);
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
