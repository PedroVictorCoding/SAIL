//! Core's process-shared admission broker. All daemon consumers use `global()`.
//! This is scheduling, not authorization or a hard OS limit. Existing runner cgroups
//! remain authoritative. External processes are reflected in fresh host measurements.
use sail_protocol::resources::{HostSample, ResourceStatus, WorkKind, WorkPriority};
use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex, OnceLock},
};
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
        parse_meminfo(
            &std::fs::read_to_string("/proc/meminfo")?,
            std::thread::available_parallelism()?.get(),
        )
    }
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
    })
}

#[derive(Debug, Clone)]
pub struct AdmissionPolicy {
    pub ram_headroom_bytes: u64,
    pub max_swap_used_bytes: u64,
    pub cpu_headroom: usize,
    pub max_heavy_models: usize,
    pub max_browsers: usize,
    pub max_builds: usize,
}
impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            ram_headroom_bytes: 2 * GIB,
            max_swap_used_bytes: GIB,
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
struct Entry {
    claim: ResourceClaim,
    cancel: CancellationToken,
}
#[derive(Default)]
struct State {
    entries: HashMap<Uuid, Entry>,
    voice_turns: usize,
    rejected: u64,
    preemptions: u64,
}
pub struct ResourceScheduler {
    probe: Arc<dyn HostProbe>,
    policy: AdmissionPolicy,
    state: Mutex<State>,
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
        }
    }
    /// Cancels background inference/builds; never kills an external model.
    /// New background work stays blocked for the entire turn, including tool/TTS gaps.
    pub fn voice_turn(self: &Arc<Self>) -> VoiceTurn {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.voice_turns += 1;
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
                return Err("live voice has priority".into());
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
            if sample.swap_used_bytes > self.policy.max_swap_used_bytes {
                return Err("host swap pressure exceeds admission limit".into());
            }
            let cpu = state
                .entries
                .values()
                .map(|e| e.claim.cpu_slots)
                .sum::<usize>();
            let capacity = sample
                .logical_cpus
                .saturating_sub(self.policy.cpu_headroom)
                .max(1);
            if cpu.saturating_add(claim.cpu_slots) > capacity {
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
            _ = cancel.cancelled() => Err(AdmissionError("Background inference preempted for live voice".into())),
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
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .voice_turns -= 1;
    }
}

impl std::fmt::Debug for ResourceScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceScheduler")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}
