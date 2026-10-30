use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareTierProfile {
    LowPower,        // 2 threads, less frequent partial transcription
    Balanced,        // 4 threads, standard 16kHz transport
    HighPerformance, // 8 threads, more frequent partial transcription
}

impl HardwareTierProfile {
    pub fn thread_count(&self) -> usize {
        match self {
            HardwareTierProfile::LowPower => 2,
            HardwareTierProfile::Balanced => 4,
            HardwareTierProfile::HighPerformance => 8,
        }
    }

    pub fn vad_chunk_ms(&self) -> usize {
        32
    }

    pub fn target_sample_rate(&self) -> u32 {
        match self {
            HardwareTierProfile::LowPower => 16000,
            HardwareTierProfile::Balanced => 16000,
            HardwareTierProfile::HighPerformance => 16000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceControlState {
    pub profile: HardwareTierProfile,
    pub is_muted: bool,
    pub active_turn: u64,
    pub auto_barge_in_enabled: bool,
}

pub struct VoiceControl {
    state: VoiceControlState,
}

impl VoiceControl {
    pub fn new() -> Self {
        Self {
            state: VoiceControlState {
                profile: HardwareTierProfile::Balanced,
                is_muted: false,
                active_turn: 0,
                auto_barge_in_enabled: true,
            },
        }
    }

    pub fn state(&self) -> &VoiceControlState {
        &self.state
    }

    pub fn set_profile(&mut self, profile: HardwareTierProfile) {
        info!(profile = ?profile, "Updated microkernel hardware tier profile");
        self.state.profile = profile;
    }

    pub fn set_muted(&mut self, muted: bool) {
        info!(muted, "Voice input mute state changed");
        self.state.is_muted = muted;
    }

    pub fn toggle_mute(&mut self) -> bool {
        self.state.is_muted = !self.state.is_muted;
        info!(is_muted = self.state.is_muted, "Voice mute toggled");
        self.state.is_muted
    }

    pub fn is_muted(&self) -> bool {
        self.state.is_muted
    }

    pub fn advance_turn(&mut self) -> u64 {
        self.state.active_turn += 1;
        self.state.active_turn
    }

    pub fn interrupt_assistant(&mut self) {
        info!(
            turn = self.state.active_turn,
            "Microkernel triggered assistant interruption"
        );
    }

    pub fn force_end_turn(&mut self) {
        info!(
            turn = self.state.active_turn,
            "Microkernel triggered force end of user turn"
        );
    }

    pub fn reset_session(&mut self) {
        info!("Microkernel reset voice session");
        self.state.active_turn = 0;
        self.state.is_muted = false;
    }
}

impl Default for VoiceControl {
    fn default() -> Self {
        Self::new()
    }
}
