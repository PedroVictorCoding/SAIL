use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub mod design_tokens {
    pub mod colors {
        pub const SAIL_BASE: &str = "#0a0c10";
        pub const SAIL_SURFACE: &str = "#141820";
        pub const SAIL_SURFACE_RAISED: &str = "#1d232c";
        pub const SAIL_GLASS_FILL: &str = "rgba(22, 29, 40, 0.65)";
        pub const SAIL_GLASS_BORDER: &str = "rgba(222, 238, 255, 0.13)";
        pub const SAIL_GLASS_HIGHLIGHT: &str = "rgba(255, 255, 255, 0.12)";
        pub const PRIMARY_TEXT: &str = "#f2f5fa";
        pub const SECONDARY_TEXT: &str = "#aeb8c8";
        pub const SUBTLE_TEXT: &str = "#8592a6";
        pub const ACCENT: &str = "#ff7a45";
        pub const FOCUS: &str = "#ffd69f";

        // Aliases for compatibility
        pub const PAGE_BG: &str = SAIL_BASE;
        pub const PANEL_SURFACE: &str = SAIL_SURFACE;
        pub const CONTROL_SURFACE: &str = SAIL_SURFACE_RAISED;
        pub const RECESSED_SURFACE: &str = "#0e121a";
    }
    pub mod depth {
        pub const GLASS_PANEL: &str = "0 16px 48px rgba(0,0,0,0.22), inset 0 1px rgba(255,255,255,0.09)";
        pub const GLASS_BORDER: &str = "1px solid rgba(222, 238, 255, 0.13)";
        pub const RAISED_HIGHLIGHT: &str = "inset 0 1px 0 rgba(255,255,255,0.12)";
        pub const RAISED_SHADOW: &str = "0 4px 12px rgba(0,0,0,0.35)";
        pub const INSET_HIGHLIGHT: &str = "inset 0 -1px 0 rgba(255,255,255,0.03)";
        pub const INSET_SHADOW: &str = "inset 0 2px 6px rgba(0,0,0,0.5)";
        pub const ACTIVE_GLOW: &str = "0 0 24px rgba(255, 122, 69, 0.35)";
    }
    pub mod layout {
        pub const CONTROL_RADIUS: u32 = 14;
        pub const CARD_RADIUS: u32 = 24;
        pub const SHEET_RADIUS: u32 = 30;
        pub const PANEL_RADIUS: u32 = 28;
    }
    pub mod motion {
        pub const FAST_TRANSITION: u64 = 140;
        pub const STATE_TRANSITION: u64 = 420;
        pub const NORMAL_TRANSITION: u64 = 200;
        pub const IDLE_QUIET_TIMEOUT_SECS: u64 = 30;
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum AmbientMode { Quiet, Active, Thinking, Speaking, Warning }

impl std::fmt::Display for AmbientMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", match self {
            AmbientMode::Quiet => "quiet", AmbientMode::Active => "active",
            AmbientMode::Thinking => "thinking", AmbientMode::Speaking => "speaking",
            AmbientMode::Warning => "warning",
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PanelState { pub panel_id: String, pub visible: bool, pub priority: u8, pub opacity: f32 }

pub struct UiSubsystem {
    bus: Option<EventBus>,
    current_mode: AmbientMode,
    panels: HashMap<String, PanelState>,
    last_activity: Arc<AtomicU64>,
}

impl UiSubsystem {
    pub fn new() -> Self {
        Self {
            bus: None,
            current_mode: AmbientMode::Active,
            panels: HashMap::new(),
            last_activity: Arc::new(AtomicU64::new(Instant::now().elapsed().as_secs())),
        }
    }
    pub fn set_ambient_mode(&mut self, mode: AmbientMode) -> Result<()> {
        if self.current_mode == mode { return Ok(()); }
        self.current_mode = mode;
        info!(?mode, "Ambient UI mode updated");
        if let Some(bus) = &self.bus {
            let event = SailEvent::new(SubsystemId::Ui,
                EventPayload::UiAmbientModeChanged { mode: mode.to_string() });
            let _ = bus.publish(event);
        }
        Ok(())
    }
    pub fn current_mode(&self) -> AmbientMode { self.current_mode }
}

impl Default for UiSubsystem { fn default() -> Self { Self::new() } }

#[async_trait]
impl Subsystem for UiSubsystem {
    fn id(&self) -> SubsystemId { SubsystemId::Ui }
    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Adaptive Ambient UI initialized"); Ok(())
    }
    async fn start(&mut self) -> Result<()> {
        info!("SAIL UI subsystem started");
        if let Some(bus) = &self.bus {
            let event = SailEvent::new(SubsystemId::Ui,
                EventPayload::SystemReady { subsystem: SubsystemId::Ui });
            let _ = bus.publish(event);
        }
        Ok(())
    }
    async fn handle_event(&self, event: &SailEvent) -> Result<()> {
        // We handle mode changes via the subsystem Arc in sail-core, simple pass through for now
        tracing::debug!(event=%event.id, "UI received event");
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL UI subsystem shutting down"); Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn initial_mode_active() { assert_eq!(UiSubsystem::new().current_mode(), AmbientMode::Active); }
    #[test] fn design_tokens_exist() { assert!(!design_tokens::colors::ACCENT.is_empty()); }
}
