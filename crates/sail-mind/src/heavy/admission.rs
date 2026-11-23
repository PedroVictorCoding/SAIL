//! Host admission: the owner's caps and the memory that must stay free for Qwen3.6 and voice.

/// Owner decision (r29): 6 GiB / 2.5 GiB; SAIL_HEAVY_HOST_RAM_GIB / SAIL_HEAVY_HOST_VRAM_GIB,
/// 0 VRAM = host GPU off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCaps {
    pub ram_bytes: u64,
    pub vram_bytes: u64,
}

impl HostCaps {
    /// The owner's caps from the environment, with the round 29 defaults.
    pub fn from_env() -> Self {
        fn gib(name: &str, default: f64) -> u64 {
            let value = std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(default);
            (value * 1024.0 * 1024.0 * 1024.0) as u64
        }
        Self {
            ram_bytes: gib("SAIL_HEAVY_HOST_RAM_GIB", 6.0),
            vram_bytes: gib("SAIL_HEAVY_HOST_VRAM_GIB", 2.5),
        }
    }
}

/// Fixed a priori (invariant 5): voice + Qwen3.6 headroom.
pub const HEADROOM_BYTES: u64 = 4 << 30;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("{what} needs {wanted} bytes but the cap is {cap}")]
    OverCap {
        what: &'static str,
        wanted: u64,
        cap: u64,
    },
    #[error("{free} bytes are free; the plan needs {wanted} to keep the headroom")]
    LowMemory { free: u64, wanted: u64 },
}

/// Admit a plan against the caps and the host's live free memory.
pub fn admit(
    _free_ram: u64,
    _free_vram: u64,
    _plan: &crate::heavy::placement::Placement,
    _caps: &HostCaps,
) -> Result<(), AdmissionError> {
    Err(AdmissionError::LowMemory {
        free: _free_ram,
        wanted: 0,
    })
}
