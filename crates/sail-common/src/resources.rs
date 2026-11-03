//! Core's process-shared admission broker. All daemon consumers use `global()`.
//! This is scheduling, not authorization or a hard OS limit. Existing runner cgroups
//! remain authoritative. External processes are reflected in fresh host measurements.
use sail_protocol::resources::{HostSample, ResourceStatus, WorkKind, WorkPriority};
use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;

pub trait HostProbe: Send + Sync {
    fn sample(&self) -> io::Result<HostSample>;
}
pub struct ProcHostProbe;
impl HostProbe for ProcHostProbe {
    fn sample(&self) -> io::Result<HostSample> {
        let mut sample = parse_meminfo(
            &std::fs::read_to_string("/proc/meminfo")?,
            std::thread::available_parallelism()?.get(),
        )?;
        sample.memory_pressure_avg10 = std::fs::read_to_string("/proc/pressure/memory")
            .ok()
            .and_then(|text| parse_memory_pressure(&text));
        Ok(sample)
    }
}
/// `some avg10` from `/proc/pressure/memory`, in percent. `None` if absent or malformed.
pub fn parse_memory_pressure(text: &str) -> Option<f32> {
    let line = text.lines().find(|l| l.starts_with("some "))?;
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse::<f32>()
        .ok()?;
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value)
}
/// Missing/malformed readings fail closed; unknown is never displayed as zero.
pub fn parse_meminfo(text: &str, logical_cpus: usize) -> io::Result<HostSample> {
    let value = |key: &str| -> io::Result<u64> {
        let mut fields = text
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .ok_or_else(|| io::Error::other(format!("missing {key}")))?
            .split_whitespace();
        let n = fields
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|n| n.checked_mul(1024))
            .ok_or_else(|| io::Error::other("invalid meminfo value"))?;
        if fields.next() != Some("kB") {
            return Err(io::Error::other("invalid meminfo unit"));
        }
        Ok(n)
    };
    let total = value("MemTotal:")?;
    let available = value("MemAvailable:")?;
    let swap_total = value("SwapTotal:")?;
    let swap_free = value("SwapFree:")?;
    if total == 0 || available > total || swap_free > swap_total || logical_cpus == 0 {
        return Err(io::Error::other("inconsistent host measurements"));
    }
    Ok(HostSample {
        available_ram_bytes: available,
        total_ram_bytes: total,
        swap_used_bytes: swap_total - swap_free,
        swap_total_bytes: swap_total,
        logical_cpus,
        memory_pressure_avg10: None,
    })
}

#[derive(Debug, Clone)]
pub struct AdmissionPolicy {
    pub ram_headroom_bytes: u64,
    /// Refuse background work while memory pressure (PSI `some avg10`) is above this.
    pub max_memory_pressure_pct: f32,
    /// Used only where the kernel reports no memory pressure. Swap in use is not itself
    /// pressure: idle pages stay swapped out long after memory is free again.
    pub max_swap_used_bytes: u64,
    pub cpu_headroom: usize,
    pub max_heavy_models: usize,
    pub max_browsers: usize,
    pub max_builds: usize,
}
impl Default for AdmissionPolicy {
    fn default() -> Self {
        let max_swap_used_bytes = std::env::var("SAIL_MAX_SWAP_USED_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(GIB);
        Self {
            ram_headroom_bytes: 2 * GIB,
            max_memory_pressure_pct: 10.0,
            max_swap_used_bytes,
            cpu_headroom: 2,
            max_heavy_models: 1,
            max_browsers: 1,
            max_builds: 1,
        }
    }
}
#[derive(Debug, Clone)]
pub struct ResourceClaim {
    pub kind: WorkKind,
    pub priority: WorkPriority,
    /// Incremental peak estimate, not RSS. Cleared after model startup succeeds.
    pub ram_bytes: u64,
    pub cpu_slots: usize,
    pub heavy_model: bool,
}
impl ResourceClaim {
    pub fn inference(priority: WorkPriority) -> Self {
        Self {
            kind: WorkKind::Inference,
            priority,
            ram_bytes: 128 * MIB,
            cpu_slots: 1,
            heavy_model: false,
        }
    }
    pub fn browser(ram_bytes: u64) -> Self {
        Self {
            kind: WorkKind::Browser,
            priority: WorkPriority::Background,
            ram_bytes,
            cpu_slots: 1,
            heavy_model: false,
        }
    }
    pub fn build() -> Self {
        Self {
            kind: WorkKind::Build,
            priority: WorkPriority::Background,
            ram_bytes: GIB,
            cpu_slots: 1,
            heavy_model: false,
        }
    }
}
#[derive(Debug, thiserror::Error)]
#[error("resource admission denied: {0}")]
pub struct AdmissionError(pub String);
/// Why background work is refused or cancelled while a voice turn runs.
const VOICE_PRIORITY: &str = "live voice has priority";
const VOICE_PREEMPTED: &str = "Background inference preempted for live voice";
impl AdmissionError {
    /// Refused or preempted only because live voice has priority: the work can resume
    /// once voice is idle ([`ResourceScheduler::voice_idle`]).
    pub fn is_voice_priority(&self) -> bool {
        self.0 == VOICE_PRIORITY || self.0 == VOICE_PREEMPTED
    }
}
struct Entry {
    claim: ResourceClaim,
    cancel: CancellationToken,
}
#[derive(Default)]
struct State {
    entries: HashMap<Uuid, Entry>,
    voice_turns: usize,
    /// Voice turns started since boot; tells a caller whether voice ran during its work.
    voice_epoch: u64,
    rejected: u64,
    preemptions: u64,
}
pub struct ResourceScheduler {
    probe: Arc<dyn HostProbe>,
    policy: AdmissionPolicy,
    state: Mutex<State>,
    /// Active voice turn count, published for `voice_idle` waiters.
    voice: watch::Sender<usize>,
}
pub fn global() -> Arc<ResourceScheduler> {
    static SCHEDULER: OnceLock<Arc<ResourceScheduler>> = OnceLock::new();
    SCHEDULER
        .get_or_init(|| {
            Arc::new(ResourceScheduler::new(
                Arc::new(ProcHostProbe),
                AdmissionPolicy::default(),
            ))
        })
        .clone()
}
impl ResourceScheduler {
    pub fn new(probe: Arc<dyn HostProbe>, policy: AdmissionPolicy) -> Self {
        Self {
            probe,
            policy,
            state: Mutex::new(State::default()),
            voice: watch::Sender::new(0),
        }
    }
    /// Cancels background inference/builds; never kills an external model.
    /// New background work stays blocked for the entire turn, including tool/TTS gaps.
    pub fn voice_turn(self: &Arc<Self>) -> VoiceTurn {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.voice_turns += 1;
        state.voice_epoch += 1;
        self.voice.send_replace(state.voice_turns);
        let mut preemptions = 0;
        for entry in state.entries.values() {
            if matches!(entry.claim.kind, WorkKind::Inference | WorkKind::Build)
                && entry.claim.priority == WorkPriority::Background
                && !entry.cancel.is_cancelled()
            {
                entry.cancel.cancel();
                preemptions += 1;
            }
        }
        state.preemptions += preemptions;
        VoiceTurn(self.clone())
    }
    /// Whether a voice turn is in progress.
    pub fn voice_active(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .voice_turns
            > 0
    }

    /// Counter of voice turns started. Compare a value taken before some background work
    /// with one taken after it to learn whether voice preempted that work.
    pub fn voice_epoch(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .voice_epoch
    }

    /// Resolves once no voice turn has started or been active for `quiet`. Background work
    /// that voice preempted waits here and then resumes, so a conversation pauses it
    /// rather than failing it. The quiet period keeps a follow-up question from being
    /// cut off by work restarting between turns.
    pub async fn voice_idle(&self, quiet: Duration) {
        let mut active = self.voice.subscribe();
        loop {
            // The sender lives as long as `self`, so this only waits for the count.
            let _ = active.wait_for(|turns| *turns == 0).await;
            let epoch = self.voice_epoch();
            tokio::time::sleep(quiet).await;
            if !self.voice_active() && self.voice_epoch() == epoch {
                return;
            }
        }
    }

    pub fn try_admit(
        self: &Arc<Self>,
        claim: ResourceClaim,
        cancel: CancellationToken,
    ) -> Result<ResourceLease, AdmissionError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let decision = (|| {
            if cancel.is_cancelled() {
                return Err("request already cancelled".into());
            }
            let sample = self
                .probe
                .sample()
                .map_err(|e| format!("host sample unavailable: {e}"))?;
            if claim.priority == WorkPriority::Background && state.voice_turns > 0 {
                return Err(VOICE_PRIORITY.into());
            }
            let pending = state
                .entries
                .values()
                .fold(0u64, |n, e| n.saturating_add(e.claim.ram_bytes));
            let needed = pending
                .saturating_add(claim.ram_bytes)
                .saturating_add(self.policy.ram_headroom_bytes);
            if sample.available_ram_bytes < needed {
                return Err("insufficient available RAM after voice/OS headroom".into());
            }
            match sample.memory_pressure_avg10 {
                Some(pressure) if pressure > self.policy.max_memory_pressure_pct => {
                    return Err(format!(
                        "host memory pressure {pressure:.1}% exceeds admission limit"
                    ));
                }
                Some(_) => {}
                None if sample.swap_used_bytes > self.policy.max_swap_used_bytes => {
                    return Err(
                        "host swap pressure exceeds admission limit (memory pressure unmeasured)"
                            .into(),
                    );
                }
                None => {}
            }
            let capacity = sample
                .logical_cpus
                .saturating_sub(self.policy.cpu_headroom)
                .max(1);
            // A claim's slots say how many threads the admitted process will use, not how many
            // exclusive lanes it owns; the OS schedules those threads. Cap the reservation at the
            // admission capacity so one process that wants the whole machine is still admissible
            // on a modest host (its thread count can exceed `logical_cpus - headroom`), while a
            // second CPU-heavy consumer is still refused.
            let reserved: usize = state
                .entries
                .values()
                .map(|e| e.claim.cpu_slots.min(capacity))
                .sum();
            if reserved.saturating_add(claim.cpu_slots.min(capacity)) > capacity {
                return Err("CPU admission slots occupied".into());
            }
            let count = |kind| {
                state
                    .entries
                    .values()
                    .filter(|e| e.claim.kind == kind)
                    .count()
            };
            if claim.kind == WorkKind::Browser
                && count(WorkKind::Browser) >= self.policy.max_browsers
            {
                return Err("browser concurrency limit reached".into());
            }
            if claim.kind == WorkKind::Build && count(WorkKind::Build) >= self.policy.max_builds {
                return Err("build concurrency limit reached".into());
            }
            if claim.heavy_model
                && state
                    .entries
                    .values()
                    .filter(|e| e.claim.heavy_model)
                    .count()
                    >= self.policy.max_heavy_models
            {
                return Err(
                    "heavy model already resident; unload it before loading another".into(),
                );
            }
            Ok(())
        })();
        if let Err(reason) = decision {
            state.rejected += 1;
            return Err(AdmissionError(reason));
        }
        let id = Uuid::new_v4();
        state.entries.insert(id, Entry { claim, cancel });
        Ok(ResourceLease {
            scheduler: self.clone(),
            id,
        })
    }
    /// Covers raw local inference clients (embeddings/review), including response bodies.
    /// The future is not polled before admission, and is dropped on voice preemption.
    pub async fn background_inference<F: std::future::Future>(
        self: &Arc<Self>,
        operation: F,
    ) -> Result<F::Output, AdmissionError> {
        let cancel = CancellationToken::new();
        let _lease = self.try_admit(
            ResourceClaim::inference(WorkPriority::Background),
            cancel.clone(),
        )?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AdmissionError(VOICE_PREEMPTED.into())),
            value = operation => Ok(value),
        }
    }

    pub fn status(&self) -> ResourceStatus {
        let sample = self.probe.sample();
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let count = |kind| {
            state
                .entries
                .values()
                .filter(|e| e.claim.kind == kind)
                .count()
        };
        ResourceStatus {
            observation_error: sample.as_ref().err().map(ToString::to_string),
            observed: sample.ok(),
            voice_turns: state.voice_turns,
            admitted_inference: count(WorkKind::Inference),
            speech_workers: count(WorkKind::Speech),
            ram_headroom_bytes: self.policy.ram_headroom_bytes,
            max_swap_used_bytes: self.policy.max_swap_used_bytes,
            resident_heavy_models: state
                .entries
                .values()
                .filter(|e| e.claim.heavy_model)
                .count(),
            active_browsers: count(WorkKind::Browser),
            active_builds: count(WorkKind::Build),
            pending_ram_bytes: state
                .entries
                .values()
                .fold(0u64, |n, e| n.saturating_add(e.claim.ram_bytes)),
            reserved_cpu_slots: state.entries.values().map(|e| e.claim.cpu_slots).sum(),
            rejected: state.rejected,
            preemptions: state.preemptions,
        }
    }
}
pub struct ResourceLease {
    scheduler: Arc<ResourceScheduler>,
    id: Uuid,
}
impl ResourceLease {
    /// Once the process is ready, its resident memory is in MemAvailable. Keep the
    /// residency/concurrency slot, but stop subtracting its startup estimate twice.
    pub fn settled(&self) {
        if let Some(entry) = self
            .scheduler
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .get_mut(&self.id)
        {
            entry.claim.ram_bytes = 0;
        }
    }
}
impl Drop for ResourceLease {
    fn drop(&mut self) {
        self.scheduler
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .remove(&self.id);
    }
}
pub struct VoiceTurn(Arc<ResourceScheduler>);
impl Drop for VoiceTurn {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.voice_turns -= 1;
        self.0.voice.send_replace(state.voice_turns);
    }
}

impl std::fmt::Debug for ResourceScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceScheduler")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}
