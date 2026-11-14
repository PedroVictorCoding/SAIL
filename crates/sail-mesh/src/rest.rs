//! When a worker steps aside for its owner.
//!
//! The worker lends a PC that someone also uses. A monitor samples how busy the PC is with
//! *other* programs (the worker's own llama-server is left out) and feeds each [`Reading`]
//! to a [`RestPolicy`]:
//!
//! - **Full rest** — a game or music software is running. New work is refused at once and
//!   the worker's llama-server is stopped, so the GPU, CPU and memory are all free again.
//! - **Backoff** — other programs are using a lot of CPU, GPU or memory. New work is refused
//!   for a while; each time the PC is still busy when the wait ends, the wait doubles (one
//!   minute, two, four… up to half an hour). Work already running finishes.
//! - **Available** — after ten calm minutes the wait starts from one minute again.
//!
//! The policy is pure (time comes in with each reading) so it can be tested without waiting;
//! the platform sampling lives in [`crate::probe`].
use sail_protocol::mesh::MeshBusy;
use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Music software that makes the worker step aside completely. Matched case-insensitively
/// against the start of a process name (without `.exe`). Audio work is sensitive to any CPU
/// spike, so these do not wait for the PC to look busy.
pub const MUSIC_APPS: &[(&str, &str)] = &[
    ("ableton live", "Ableton Live"),
    ("fl64", "FL Studio"),
    ("fl studio", "FL Studio"),
    ("reaper", "REAPER"),
    ("cubase", "Cubase"),
    ("nuendo", "Nuendo"),
    ("studio one", "Studio One"),
    ("bitwig", "Bitwig Studio"),
    ("protools", "Pro Tools"),
    ("reason", "Reason"),
    ("ardour", "Ardour"),
    ("lmms", "LMMS"),
    ("renoise", "Renoise"),
    ("cakewalk", "Cakewalk"),
    ("waveform", "Waveform"),
    ("adobe audition", "Adobe Audition"),
    ("maschine", "Maschine"),
    ("serato", "Serato"),
    ("rekordbox", "rekordbox"),
    ("traktor", "Traktor"),
];

/// Thresholds and timings of the policy.
#[derive(Debug, Clone)]
pub struct RestLimits {
    /// Other programs' share of all CPU cores (0–100) that counts as busy.
    pub cpu_percent: f32,
    /// Other programs' GPU use (0–100) that counts as busy.
    pub gpu_percent: f32,
    /// Other programs' GPU use that counts as gaming: full rest.
    pub gpu_full_percent: f32,
    /// Free memory below this (MiB) counts as busy.
    pub min_free_ram_mb: u64,
    /// First backoff; each further one doubles.
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    /// Calm time after which the backoff starts from `base_backoff` again.
    pub calm_reset: Duration,
    /// Consecutive busy readings before backing off, so one spike (opening a program) does
    /// not count. Process and full-screen detection act on the first reading.
    pub busy_readings: u32,
}

impl Default for RestLimits {
    fn default() -> Self {
        Self {
            cpu_percent: 50.0,
            gpu_percent: 20.0,
            gpu_full_percent: 50.0,
            min_free_ram_mb: 2048,
            base_backoff: Duration::from_secs(60),
            max_backoff: Duration::from_secs(30 * 60),
            calm_reset: Duration::from_secs(10 * 60),
            busy_readings: 2,
        }
    }
}

/// One sample of how the PC is used by programs other than the worker.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    /// Share of all CPU cores (0–100) used by other programs.
    pub other_cpu_percent: f32,
    /// GPU use (0–100) by other programs, when it could be measured.
    pub other_gpu_percent: Option<f32>,
    /// Free memory in MiB, when known.
    pub free_ram_mb: Option<u64>,
    /// Something that always means full rest (a music app, a full-screen game, a program
    /// the owner listed), described for the owner.
    pub exclusive: Option<String>,
    /// A full-screen window that is not known to be a game (a video, a presentation).
    pub full_screen: bool,
}

/// What the worker does right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rest {
    Available,
    /// Refuse new work until `until`.
    Backoff {
        until: Instant,
        reason: String,
    },
    /// Refuse all work and stop the model server.
    Full {
        reason: String,
    },
}

/// The exponential-backoff state machine.
#[derive(Debug)]
pub struct RestPolicy {
    limits: RestLimits,
    state: Rest,
    /// Backoffs in a row without a calm reset; the next wait is `base * 2^level`.
    level: u32,
    busy_streak: u32,
    calm_since: Option<Instant>,
}

impl RestPolicy {
    pub fn new(limits: RestLimits) -> Self {
        Self {
            limits,
            state: Rest::Available,
            level: 0,
            busy_streak: 0,
            calm_since: None,
        }
    }

    pub fn state(&self) -> &Rest {
        &self.state
    }

    /// How long the next backoff would last.
    pub fn next_backoff(&self) -> Duration {
        let factor = 1u32.checked_shl(self.level.min(20)).unwrap_or(u32::MAX);
        self.limits
            .base_backoff
            .saturating_mul(factor)
            .min(self.limits.max_backoff)
    }

    fn back_off(&mut self, now: Instant, reason: String) {
        let wait = self.next_backoff();
        self.level = self.level.saturating_add(1).min(20);
        self.busy_streak = 0;
        self.calm_since = None;
        self.state = Rest::Backoff {
            until: now + wait,
            reason,
        };
    }

    /// Why this reading means the PC is in heavy, game-like use, if it does.
    fn heavy(&self, reading: &Reading) -> Option<String> {
        let gpu = reading.other_gpu_percent?;
        (gpu >= self.limits.gpu_full_percent).then(|| {
            if reading.full_screen {
                format!("gaming (full-screen app using {gpu:.0}% GPU)")
            } else {
                format!("gaming or other heavy GPU use ({gpu:.0}% GPU)")
            }
        })
    }

    /// Why this reading means the PC is busy, if it does.
    fn busy_reason(&self, reading: &Reading) -> Option<String> {
        if reading.other_cpu_percent >= self.limits.cpu_percent {
            return Some(format!(
                "other programs use {:.0}% CPU",
                reading.other_cpu_percent
            ));
        }
        if let Some(gpu) = reading.other_gpu_percent
            && gpu >= self.limits.gpu_percent
        {
            return Some(format!("other programs use {gpu:.0}% GPU"));
        }
        if let Some(free) = reading.free_ram_mb
            && free < self.limits.min_free_ram_mb
        {
            return Some(format!("low memory ({free} MiB free)"));
        }
        if reading.full_screen {
            return Some("a full-screen app is open".into());
        }
        None
    }

    /// Take one reading and return what the worker should do now.
    pub fn observe(&mut self, reading: &Reading, now: Instant) -> &Rest {
        if let Some(reason) = reading.exclusive.clone() {
            self.busy_streak = 0;
            self.calm_since = None;
            self.state = Rest::Full { reason };
            return &self.state;
        }
        // Heavy GPU use is treated like a game, but only once it lasts (not one spike).
        let heavy = self.heavy(reading);
        let busy = heavy.clone().or_else(|| self.busy_reason(reading));
        let is_busy = busy.is_some();
        if is_busy {
            self.busy_streak = self.busy_streak.saturating_add(1);
        } else {
            self.busy_streak = 0;
        }
        let lasted = self.busy_streak >= self.limits.busy_readings.max(1);
        if let (Some(reason), true) = (heavy, lasted) {
            self.calm_since = None;
            self.state = Rest::Full { reason };
            return &self.state;
        }
        match self.state.clone() {
            Rest::Full { reason } => {
                // The game or music app just closed: wait before loading the model again, so
                // a quick break (a loading screen, a menu) does not start and stop it.
                self.back_off(now, format!("{reason} just ended"));
            }
            Rest::Backoff { until, .. } if now < until => {}
            Rest::Backoff { .. } => match busy {
                // Still busy when the wait ends: wait twice as long.
                Some(reason) => self.back_off(now, reason),
                None => {
                    self.state = Rest::Available;
                    self.calm_since = Some(now);
                }
            },
            Rest::Available => match busy {
                Some(reason) if lasted => self.back_off(now, reason),
                Some(_) => {}
                None => {
                    let calm_since = *self.calm_since.get_or_insert(now);
                    if now.saturating_duration_since(calm_since) >= self.limits.calm_reset {
                        self.level = 0;
                    }
                }
            },
        }
        if is_busy {
            self.calm_since = None;
        }
        &self.state
    }

    /// The state as the host sees it.
    pub fn busy(&self, now: Instant) -> Option<MeshBusy> {
        match &self.state {
            Rest::Available => None,
            Rest::Backoff { until, reason } => Some(MeshBusy {
                reason: reason.clone(),
                full: false,
                retry_after_secs: Some(until.saturating_duration_since(now).as_secs().max(1)),
            }),
            Rest::Full { reason } => Some(MeshBusy {
                reason: reason.clone(),
                full: true,
                retry_after_secs: None,
            }),
        }
    }
}

/// The music app a process name belongs to, if any.
pub fn music_app(process_name: &str) -> Option<&'static str> {
    let name = normalise(process_name);
    MUSIC_APPS
        .iter()
        .find(|(prefix, _)| name.starts_with(prefix))
        .map(|(_, label)| *label)
}

/// Lower-case process name without a `.exe` suffix.
pub fn normalise(process_name: &str) -> String {
    let lower = process_name.trim().to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_owned()
}

/// State shared between the usage monitor and the worker's request handler.
#[derive(Debug, Clone, Default)]
pub struct RestGate {
    inner: Arc<GateInner>,
}

#[derive(Debug, Default)]
struct GateInner {
    busy: RwLock<Option<MeshBusy>>,
    active: AtomicUsize,
}

impl RestGate {
    /// Why new work is refused right now, if it is.
    pub fn busy(&self) -> Option<MeshBusy> {
        self.inner
            .busy
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    pub fn set_busy(&self, busy: Option<MeshBusy>) {
        *self.inner.busy.write().unwrap_or_else(|p| p.into_inner()) = busy;
    }
    /// Generations running on the worker now.
    pub fn active(&self) -> usize {
        self.inner.active.load(Ordering::SeqCst)
    }
    pub(crate) fn started(&self) {
        self.inner.active.fetch_add(1, Ordering::SeqCst);
    }
    pub(crate) fn finished(&self) {
        self.inner.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calm() -> Reading {
        Reading {
            other_cpu_percent: 5.0,
            other_gpu_percent: Some(0.0),
            free_ram_mb: Some(16_000),
            ..Reading::default()
        }
    }
    fn cpu(percent: f32) -> Reading {
        Reading {
            other_cpu_percent: percent,
            ..calm()
        }
    }
    fn gpu(percent: f32) -> Reading {
        Reading {
            other_gpu_percent: Some(percent),
            ..calm()
        }
    }
    const S: Duration = Duration::from_secs(1);
    const MIN: Duration = Duration::from_secs(60);

    fn backoff_left(policy: &RestPolicy, now: Instant) -> Option<Duration> {
        match policy.state() {
            Rest::Backoff { until, .. } => Some(until.saturating_duration_since(now)),
            _ => None,
        }
    }

    #[test]
    fn one_spike_is_ignored_but_lasting_load_backs_off() {
        let t = Instant::now();
        let mut policy = RestPolicy::new(RestLimits::default());
        assert_eq!(policy.observe(&cpu(90.0), t), &Rest::Available);
        assert_eq!(policy.observe(&calm(), t + 5 * S), &Rest::Available);
        assert_eq!(policy.observe(&cpu(90.0), t + 10 * S), &Rest::Available);
        policy.observe(&cpu(90.0), t + 15 * S);
        assert_eq!(backoff_left(&policy, t + 15 * S), Some(MIN));
        let busy = policy.busy(t + 15 * S).unwrap();
        assert!(!busy.full);
        assert_eq!(busy.retry_after_secs, Some(60));
        assert!(busy.reason.contains("90% CPU"), "{}", busy.reason);
    }

    #[test]
    fn backoff_doubles_while_busy_and_resets_after_calm() {
        let t = Instant::now();
        let mut policy = RestPolicy::new(RestLimits::default());
        policy.observe(&cpu(80.0), t);
        policy.observe(&cpu(80.0), t);
        assert_eq!(backoff_left(&policy, t), Some(MIN));
        // Readings inside the wait change nothing.
        policy.observe(&calm(), t + 30 * S);
        assert_eq!(backoff_left(&policy, t + 30 * S), Some(30 * S));
        // Still busy when the wait ends: 2, 4, 8 … minutes, capped at 30.
        let mut now = t + MIN;
        for expected in [2, 4, 8, 16, 30, 30] {
            policy.observe(&cpu(80.0), now);
            assert_eq!(backoff_left(&policy, now), Some(expected * MIN));
            now += expected * MIN;
        }
        // Calm when the wait ends: available, but the next wait stays long for a while.
        assert_eq!(policy.observe(&calm(), now), &Rest::Available);
        assert_eq!(policy.next_backoff(), 30 * MIN);
        policy.observe(&calm(), now + 9 * MIN);
        assert_eq!(policy.next_backoff(), 30 * MIN);
        policy.observe(&calm(), now + 10 * MIN);
        assert_eq!(
            policy.next_backoff(),
            MIN,
            "ten calm minutes reset the backoff"
        );
    }

    #[test]
    fn busy_reading_interrupts_the_calm_period() {
        let t = Instant::now();
        let mut policy = RestPolicy::new(RestLimits::default());
        policy.observe(&cpu(80.0), t);
        policy.observe(&cpu(80.0), t);
        policy.observe(&calm(), t + MIN);
        policy.observe(&cpu(80.0), t + MIN + 5 * MIN);
        policy.observe(&calm(), t + MIN + 6 * MIN);
        policy.observe(&calm(), t + MIN + 12 * MIN);
        assert_eq!(
            policy.next_backoff(),
            2 * MIN,
            "a busy blip restarts the calm clock"
        );
        policy.observe(&calm(), t + MIN + 16 * MIN);
        assert_eq!(policy.next_backoff(), MIN);
    }

    #[test]
    fn music_app_means_full_rest_at_once_then_a_backoff() {
        let t = Instant::now();
        let mut policy = RestPolicy::new(RestLimits::default());
        let music = Reading {
            exclusive: Some("making music (Ableton Live)".into()),
            ..calm()
        };
        assert_eq!(
            policy.observe(&music, t),
            &Rest::Full {
                reason: "making music (Ableton Live)".into()
            }
        );
        let busy = policy.busy(t).unwrap();
        assert!(busy.full);
        assert_eq!(busy.retry_after_secs, None);
        policy.observe(&music, t + 60 * MIN);
        // Closed: back off before loading the model again.
        policy.observe(&calm(), t + 61 * MIN);
        assert_eq!(backoff_left(&policy, t + 61 * MIN), Some(MIN));
        assert!(
            policy
                .busy(t + 61 * MIN)
                .unwrap()
                .reason
                .ends_with("just ended")
        );
        assert_eq!(policy.observe(&calm(), t + 62 * MIN), &Rest::Available);
    }

    #[test]
    fn heavy_gpu_use_counts_as_gaming_once_it_lasts() {
        let t = Instant::now();
        let mut policy = RestPolicy::new(RestLimits::default());
        assert_eq!(policy.observe(&gpu(85.0), t), &Rest::Available);
        assert!(matches!(
            policy.observe(&gpu(85.0), t + 5 * S),
            Rest::Full { reason } if reason.contains("gaming")
        ));
        // Light GPU use (a video) is only a backoff.
        let mut policy = RestPolicy::new(RestLimits::default());
        policy.observe(&gpu(30.0), t);
        policy.observe(&gpu(30.0), t + 5 * S);
        assert!(matches!(policy.state(), Rest::Backoff { .. }));
    }

    #[test]
    fn full_screen_with_gpu_load_is_gaming_without_is_a_backoff() {
        let t = Instant::now();
        let game = Reading {
            full_screen: true,
            other_gpu_percent: Some(70.0),
            ..calm()
        };
        let mut policy = RestPolicy::new(RestLimits::default());
        policy.observe(&game, t);
        assert!(matches!(
            policy.observe(&game, t + 5 * S),
            Rest::Full { reason } if reason.starts_with("gaming (full-screen")
        ));
        let video = Reading {
            full_screen: true,
            ..calm()
        };
        let mut policy = RestPolicy::new(RestLimits::default());
        policy.observe(&video, t);
        policy.observe(&video, t + 5 * S);
        assert!(matches!(policy.state(), Rest::Backoff { .. }));
    }

    #[test]
    fn low_memory_backs_off() {
        let t = Instant::now();
        let mut policy = RestPolicy::new(RestLimits::default());
        let low = Reading {
            free_ram_mb: Some(900),
            ..calm()
        };
        policy.observe(&low, t);
        policy.observe(&low, t + 5 * S);
        assert!(policy.busy(t).unwrap().reason.contains("low memory"));
    }

    #[test]
    fn music_apps_match_windows_and_linux_names() {
        for (name, label) in [
            ("Ableton Live 12 Suite.exe", "Ableton Live"),
            ("FL64.exe", "FL Studio"),
            ("reaper.exe", "REAPER"),
            ("Cubase14.exe", "Cubase"),
            ("Studio One.exe", "Studio One"),
            ("BitwigStudio.exe", "Bitwig Studio"),
            ("ardour8", "Ardour"),
        ] {
            assert_eq!(music_app(name), Some(label), "{name}");
        }
        for name in [
            "chrome.exe",
            "Steam.exe",
            "explorer.exe",
            "llama-server.exe",
        ] {
            assert_eq!(music_app(name), None, "{name}");
        }
    }

    #[test]
    fn gate_is_shared_between_clones() {
        let gate = RestGate::default();
        let other = gate.clone();
        assert_eq!(other.busy(), None);
        gate.set_busy(Some(MeshBusy {
            reason: "x".into(),
            full: true,
            retry_after_secs: None,
        }));
        assert!(other.busy().unwrap().full);
        gate.started();
        assert_eq!(other.active(), 1);
        gate.finished();
        assert_eq!(other.active(), 0);
    }
}
