pub mod address;
pub mod approval_http;
pub mod briefing;
pub mod briefing_http;
pub mod capital_http;
pub mod capital_retirement;
mod capital_tools;
pub mod connection_controller;
pub mod control;
pub mod conversation_controller;
#[cfg(test)]
mod conversation_controller_tests;
mod curiosity_runner;
pub mod dictation_http;
mod escalation_outcomes;
mod execution;
pub mod flow_http;
pub mod gateway_http;
pub mod gateway_service;
pub mod greeting;
pub mod hermes_http;
pub mod hermes_tools;
mod html_text;
pub mod learn_courses_http;
pub mod learn_factory_http;
pub mod learn_http;
pub mod learn_pdf;
pub mod live_context;
pub mod market_http;
pub mod memory_http;
pub mod memory_profile;
pub mod mesh_http;
pub mod mind_models_http;
pub mod model_eval;
pub mod moodle_http;
pub mod moodle_model;
mod moodle_tools;
pub mod music_fastpath;
pub mod music_http;
pub mod ogg_opus;
pub mod orb;
pub mod pipeline;
pub mod plans;
pub mod proactive;
pub mod proactive_http;
pub mod prompt;
pub mod proxy_guard;
pub mod pulse_http;
pub mod reader_http;
pub mod schedule_http;
pub mod schedule_runner;
pub mod server;
mod speech;
pub mod spotify_http;
pub mod staged_ack;
pub mod stt;
pub mod system_http;
pub mod terminal;
mod tool_policy;
pub mod tracker_http;
pub mod tts;
pub mod turn_coordinator;
pub mod ui_assets;
pub mod vad;
mod vision_archive;
pub mod visual_snapshot;
pub mod vlm;
pub mod web_assets;

use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

pub use control::{
    HardwareTierProfile, TurnDetectionSettings, TurnMode, VoiceControl, VoiceControlState,
};
pub use execution::execute_escalation;
pub use orb::{AudioSpectrum8Band, OrbEngine, OrbState, OrbTelemetry};
pub use pipeline::RealtimeVoicePipeline;
pub use server::run_dialogue_server;
pub use speech::SpeechChunker;
pub use staged_ack::StagedAckCatalog;
pub use stt::{LocalStreamingStt, StreamingToken, SttConfig};
pub use tts::{LocalNeuralTts, TtsConfig};
pub use turn_coordinator::{FAST_PATH_LATENCY_THRESHOLD_MS, StagedToolOutcome, TurnCoordinator};
pub use vad::{SileroVad, SileroVadConfig, VadEvent, VadTelemetry};
pub use vlm::{
    KernelToolCall, KernelToolResult, LocalKernelToolBroker, VisionAnalysis, VisionFrameInput,
    VoiceVlm,
};

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct DialogueSubsystem {
    bus: Option<EventBus>,
    server_addr: Option<SocketAddr>,
    services: sail_mind::VoiceModelServices,
    arena: Arc<sail_arena::ArenaSubsystem>,
    plans: Option<Arc<plans::Plans>>,
    pulse: Option<Arc<sail_pulse::Pulse>>,
}

impl DialogueSubsystem {
    pub fn new() -> Self {
        Self {
            bus: None,
            server_addr: Some(
                std::env::var("SAIL_VOICE_BIND")
                    .unwrap_or("127.0.0.1:8088".into())
                    .parse()
                    .expect("Invalid SAIL_VOICE_BIND"),
            ),
            services: sail_mind::VoiceModelServices::new(),
            arena: Arc::new(sail_arena::ArenaSubsystem::open_default()),
            plans: None,
            pulse: None,
        }
    }

    pub fn with_model_services(mut self, services: sail_mind::VoiceModelServices) -> Self {
        self.services = services;
        self
    }
    pub fn with_arena(mut self, arena: Arc<sail_arena::ArenaSubsystem>) -> Self {
        self.arena = arena;
        self
    }
    /// Agent task plans: "Plan: …" turns and the plan card.
    pub fn with_plans(mut self, plans: Arc<plans::Plans>) -> Self {
        self.plans = Some(plans);
        self
    }
    /// Local reminders and the server-owned scheduler (`docs/SAIL_PULSE.md`).
    pub fn with_pulse(mut self, pulse: Arc<sail_pulse::Pulse>) -> Self {
        self.pulse = Some(pulse);
        self
    }
    pub fn with_addr(mut self, addr: SocketAddr) -> Self {
        self.server_addr = Some(addr);
        self
    }
}

impl Default for DialogueSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for DialogueSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Dialogue
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!(
            "SAIL Dialogue initialized (voice:vad + voice:stt + voice:vlm + voice:tts + voice:orb + voice:control with offline model workers)"
        );
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Dialogue subsystem started");
        if let Some(bus) = &self.bus {
            let event = SailEvent::new(
                self.id(),
                EventPayload::SystemReady {
                    subsystem: self.id(),
                },
            );
            let _ = bus.publish(event);

            if let Some(addr) = self.server_addr {
                let bus_clone = bus.clone();
                let services = self.services.clone();
                let arena = self.arena.clone();
                let plans = self.plans.clone();
                let pulse = self.pulse.clone();
                tokio::spawn(async move {
                    if let Err(err) = server::run_dialogue_server_with_plans(
                        addr, bus_clone, services, arena, plans, pulse,
                    )
                    .await
                    {
                        tracing::error!(error = %err, "Dialogue Orb server encountered error");
                    }
                });
                info!(addr = %addr, "Voice Orb client web UI available");
            }
        }
        Ok(())
    }

    async fn handle_event(&self, event: &SailEvent) -> Result<()> {
        match &event.payload {
            EventPayload::DialogueInput {
                session_id,
                speaker,
                content,
                is_final,
            } => {
                info!(
                    session_id = %session_id,
                    speaker = %speaker,
                    content_chars = content.chars().count(),
                    is_final = %is_final,
                    "SAIL Dialogue processing speech turn -> routing to Agents"
                );
            }
            EventPayload::DialogueResponseDelta {
                session_id,
                delta,
                is_finished,
            } => {
                info!(
                    session_id = %session_id,
                    delta = %delta,
                    is_finished = %is_finished,
                    "SAIL Dialogue received routed response event"
                );
            }
            EventPayload::DialogueInterruption { session_id } => {
                info!(session_id = %session_id, "SAIL Dialogue received interruption signal");
            }
            _ => {}
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Dialogue subsystem shutting down");
        Ok(())
    }
}

/// Convert 16-bit PCM samples into standard RIFF WAV format bytes (zero external APIs)
pub fn stt_wav_encode(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    let file_len = 36 + data_len;

    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&file_len.to_le_bytes());
    header.extend_from_slice(b"WAVE");

    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * 2;
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&2u16.to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes());

    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());

    for &sample in samples {
        header.extend_from_slice(&sample.to_le_bytes());
    }

    header
}
