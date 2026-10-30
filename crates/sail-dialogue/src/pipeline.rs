use crate::{
    control::{HardwareTierProfile, VoiceControl, VoiceControlState},
    orb::{OrbEngine, OrbState, OrbTelemetry},
    stt::LocalStreamingStt,
    tts::{LocalNeuralTts, TtsConfig},
    vad::{SileroVad, SileroVadConfig, VadEvent, VadTelemetry},
    vlm::VoiceVlm,
};
use sail_common::error::Result;
use sail_mind::VoiceModelServices;
use std::sync::Arc;

pub struct RealtimeVoicePipeline {
    vad: SileroVad,
    orb: OrbEngine,
    control: VoiceControl,
    pub stt: LocalStreamingStt,
    pub tts: LocalNeuralTts,
    pub vlm: Arc<VoiceVlm>,
}
impl Default for RealtimeVoicePipeline {
    fn default() -> Self {
        Self::new()
    }
}
impl RealtimeVoicePipeline {
    pub fn new() -> Self {
        Self::with_services(VoiceModelServices::new(), uuid::Uuid::new_v4().to_string())
    }
    pub fn with_services(services: VoiceModelServices, session: String) -> Self {
        let vlm = Arc::new(VoiceVlm::with_services(&services));
        Self {
            vad: SileroVad::with_services(SileroVadConfig::default(), services.clone(), session),
            orb: OrbEngine::new(),
            control: VoiceControl::new(),
            stt: LocalStreamingStt::with_services(services.clone()),
            tts: LocalNeuralTts::with_services(TtsConfig::default(), services),
            vlm,
        }
    }
    pub fn orb_state(&self) -> OrbState {
        self.orb.state()
    }
    pub fn set_orb_state(&mut self, state: OrbState) {
        self.orb.set_state(state);
    }
    pub fn control_state(&self) -> &VoiceControlState {
        self.control.state()
    }
    pub fn set_hardware_profile(&mut self, profile: HardwareTierProfile) {
        self.control.set_profile(profile);
        self.stt.threads = profile.thread_count();
        self.tts.threads = profile.thread_count();
    }
    pub fn toggle_mute(&mut self) -> bool {
        self.vad.reset();
        self.control.toggle_mute()
    }
    pub fn interrupt(&mut self) {
        self.vad.reset();
        self.control.interrupt_assistant();
        self.orb.set_state(OrbState::Interrupted);
    }
    pub fn advance_turn(&mut self) -> u64 {
        self.control.advance_turn()
    }
    pub fn snapshot(&self) -> Option<Vec<i16>> {
        self.vad.snapshot()
    }
    pub fn force_end_turn(&mut self) -> Option<Vec<i16>> {
        self.control.force_end_turn();
        self.vad.finish()
    }
    pub async fn ingest_frame(
        &mut self,
        samples: &[i16],
    ) -> Result<(Option<Vec<i16>>, VadTelemetry, OrbTelemetry)> {
        if self.control.is_muted() {
            return Ok((
                None,
                VadTelemetry {
                    event: VadEvent::Silence,
                    confidence: 0.,
                    rms_power: 0.,
                    zero_crossing_rate: 0.,
                    spectral_centroid: 0.,
                    process_duration_us: 0,
                    inference_us: 0,
                },
                self.orb.compute_telemetry(&[], 16000),
            ));
        }
        let (event, audio, tel) = self.vad.process_chunk(samples).await?;
        match event {
            VadEvent::SpeechStart | VadEvent::InSpeech => {
                self.orb.set_state(OrbState::Listening);
                self.orb.set_speaker("user");
            }
            VadEvent::SpeechEnd => {
                self.orb.set_state(OrbState::Thinking);
                self.orb.set_speaker("assistant");
            }
            _ => {}
        }
        let orb = self.orb.compute_telemetry(samples, 16000);
        Ok((audio, tel, orb))
    }
    pub fn reset_capture(&mut self) {
        self.vad.reset();
        self.orb.set_state(OrbState::Thinking);
    }
    pub fn reset(&mut self) {
        self.vad.reset();
        self.control.reset_session();
        self.orb.set_state(OrbState::Idle);
        self.orb.set_transcript("");
        self.orb.set_speaker("none");
    }
}
