use async_trait::async_trait;
use std::collections::HashMap;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

/// Ambient UI display mode - adapts to user attention and system state
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum AmbientMode {
    /// Idle quiet state: minimal glow, no distractions
    Quiet,
    /// Active presence: normal dashboard visibility
    Active,
    /// Thinking: subtle pulse animation
    Thinking,
    /// Speaking: clear voice waveform indicators
    Speaking,
    /// Error: warning glow with explicit state
    Warning,
}

impl std::fmt::Display for AmbientMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", match self {
            AmbientMode::Quiet => "quiet",
            AmbientMode::Active => "active",
            AmbientMode::Thinking => "thinking",
            AmbientMode::Speaking => "speaking",
            AmbientMode::Warning => "warning",
        })
    }
}

/// Adaptive panel positioning and visibility
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PanelState {
    pub panel_id: String,
    pub visible: bool,
    pub priority: u8,
    pub opacity: f32,
    pub last_interaction_ms: u64,
}

/// Adaptive ambient UI subsystem - handles dynamic UI state based on system and user context
pub struct UiSubsystem {
    bus: Option<EventBus>,
    current_mode: AmbientMode,
    panels: HashMap<String, PanelState>,
}

impl UiSubsystem {
    pub fn new() -> Self {
        Self {
            bus: None,
            current_mode: AmbientMode::Quiet,
            panels: HashMap::new(),
        }
    }

    /// Update ambient mode based on current system state
    pub fn set_ambient_mode(&mut self, mode: AmbientMode) -> Result<()> {
        self.current_mode = mode;
        info!(?mode, "Ambient UI mode updated");
        if let Some(bus) = &self.bus {
            let event = SailEvent::new(
                SubsystemId::Ui,
                EventPayload::UiAmbientModeChanged { mode: mode.to_string() },
            );
            let _ = bus.publish(event);
        }
        Ok(())
    }

    /// Get current ambient UI mode
    pub fn current_mode(&self) -> AmbientMode {
        self.current_mode
    }
}

impl Default for UiSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for UiSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Ui
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Adaptive Ambient UI initialized");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL UI subsystem started");
        if let Some(bus) = &self.bus {
            let event = SailEvent::new(
                self.id(),
                EventPayload::SystemReady {
                    subsystem: self.id(),
                },
            );
            let _ = bus.publish(event);
        }
        Ok(())
    }

    async fn handle_event(&self, _event: &SailEvent) -> Result<()> {
        // TODO: React to system state changes to adapt UI automatically
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL UI subsystem shutting down");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_subsystem_initializes() {
        let ui = UiSubsystem::new();
        assert_eq!(ui.current_mode(), AmbientMode::Quiet);
    }
}
