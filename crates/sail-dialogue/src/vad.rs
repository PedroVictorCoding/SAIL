use crate::control::{MAX_RECORDING_MS, TurnDetectionSettings, TurnMode};
use sail_common::error::{Result, SailError};
use sail_mind::VoiceModelServices;
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, time::Instant};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SileroVadConfig {
    pub sample_rate: usize,
    pub threshold: f32,
    pub min_speech_duration_ms: usize,
    pub min_silence_duration_ms: usize,
    pub speech_pad_ms: usize,
    pub turn_mode: TurnMode,
    pub max_turn_duration_ms: usize,
}
impl Default for SileroVadConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            threshold: 0.5,
            min_speech_duration_ms: 128,
            min_silence_duration_ms: TurnDetectionSettings::default().pause_ms,
            speech_pad_ms: 96,
            turn_mode: TurnDetectionSettings::default().mode,
            max_turn_duration_ms: MAX_RECORDING_MS,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VadEvent {
    Silence,
    SpeechStart,
    InSpeech,
    SpeechEnd,
    CaptureLimitReached,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadTelemetry {
    pub event: VadEvent,
    pub confidence: f32,
    pub rms_power: f32,
    pub zero_crossing_rate: f32,
    pub spectral_centroid: f32,
    pub process_duration_us: u64,
    pub inference_us: u64,
    pub captured_ms: usize,
}
pub struct SileroVad {
    config: SileroVadConfig,
    services: VoiceModelServices,
    session: String,
    speaking: bool,
    speech_samples: usize,
    silence_samples: usize,
    audio: Vec<i16>,
    padding: VecDeque<i16>,
    capture_full: bool,
}
impl SileroVad {
    pub fn new(config: SileroVadConfig) -> Self {
        Self::with_services(
            config,
            VoiceModelServices::new(),
            uuid::Uuid::new_v4().to_string(),
        )
    }
    pub fn with_services(
        config: SileroVadConfig,
        services: VoiceModelServices,
        session: String,
    ) -> Self {
        Self {
            config,
            services,
            session,
            speaking: false,
            speech_samples: 0,
            silence_samples: 0,
            audio: Vec::new(),
            padding: VecDeque::new(),
            capture_full: false,
        }
    }
    pub async fn process_chunk(
        &mut self,
        samples: &[i16],
    ) -> Result<(VadEvent, Option<Vec<i16>>, VadTelemetry)> {
        if samples.len() != 512 || self.config.sample_rate != 16000 {
            return Err(SailError::Config(
                "Silero input must be 512 mono samples at 16kHz".into(),
            ));
        }
        let start = Instant::now();
        let (probability, inference_us) = if self.capture_full {
            (0., 0)
        } else {
            self.services.vad(&self.session, samples).await?
        };
        let (event, audio, mut telemetry) = self.process_probability(samples, probability);
        telemetry.process_duration_us = start.elapsed().as_micros() as u64;
        telemetry.inference_us = inference_us;
        Ok((event, audio, telemetry))
    }
    /// State machine isolated from inference for deterministic boundary tests.
    pub fn process_probability(
        &mut self,
        samples: &[i16],
        probability: f32,
    ) -> (VadEvent, Option<Vec<i16>>, VadTelemetry) {
        let threshold = if self.speaking {
            self.config.threshold - 0.15
        } else {
            self.config.threshold
        };
        let event;
        if self.capture_full {
            event = VadEvent::Silence;
        } else if probability >= threshold {
            if self.audio.is_empty() {
                self.audio.extend(self.padding.iter());
            }
            self.audio.extend_from_slice(samples);
            self.speech_samples += samples.len();
            self.silence_samples = 0;
            if !self.speaking && self.speech_samples >= self.config.min_speech_duration_ms * 16 {
                self.speaking = true;
                event = VadEvent::SpeechStart;
            } else {
                event = if self.speaking {
                    VadEvent::InSpeech
                } else {
                    VadEvent::Silence
                };
            }
        } else if self.speaking {
            self.audio.extend_from_slice(samples);
            self.silence_samples += samples.len();
            event = if self.config.turn_mode == TurnMode::Automatic
                && self.silence_samples >= self.config.min_silence_duration_ms * 16
            {
                VadEvent::SpeechEnd
            } else {
                VadEvent::InSpeech
            };
        } else {
            self.audio.clear();
            self.speech_samples = 0;
            self.silence_samples = 0;
            event = VadEvent::Silence;
        }
        self.padding.extend(samples);
        while self.padding.len() > self.config.speech_pad_ms * 16 {
            self.padding.pop_front();
        }
        let limit_samples = self
            .config
            .max_turn_duration_ms
            .clamp(1000, MAX_RECORDING_MS)
            * 16;
        let event = if !self.capture_full
            && self.speaking
            && self.audio.len() >= limit_samples
            && event != VadEvent::SpeechEnd
        {
            self.silence_samples = self
                .silence_samples
                .saturating_sub(self.audio.len() - limit_samples);
            self.audio.truncate(limit_samples);
            self.capture_full = true;
            VadEvent::CaptureLimitReached
        } else {
            event
        };
        let captured_ms = self.audio.len() / 16;
        let audio = if event == VadEvent::SpeechEnd {
            self.trim_trailing_silence();
            self.speaking = false;
            self.speech_samples = 0;
            self.silence_samples = 0;
            Some(std::mem::take(&mut self.audio))
        } else {
            None
        };
        let rms = if samples.is_empty() {
            0.
        } else {
            (samples
                .iter()
                .map(|s| (*s as f64 / 32768.).powi(2))
                .sum::<f64>()
                / samples.len() as f64)
                .sqrt() as f32
        };
        let zcr = samples
            .windows(2)
            .filter(|p| (p[0] < 0) != (p[1] < 0))
            .count() as f32
            / samples.len().max(1) as f32;
        (
            event,
            audio,
            VadTelemetry {
                event,
                confidence: probability.clamp(0., 1.),
                rms_power: rms,
                zero_crossing_rate: zcr,
                spectral_centroid: 0.,
                process_duration_us: 0,
                inference_us: 0,
                captured_ms,
            },
        )
    }
    pub fn snapshot(&self) -> Option<Vec<i16>> {
        if self.speaking && !self.capture_full && self.audio.len() <= 30 * 16000 {
            Some(self.audio.clone())
        } else {
            None
        }
    }
    pub fn finish(&mut self) -> Option<Vec<i16>> {
        self.trim_trailing_silence();
        let audio = if self.speaking {
            Some(std::mem::take(&mut self.audio))
        } else {
            None
        };
        self.reset();
        audio
    }
    pub fn set_turn_detection(&mut self, settings: TurnDetectionSettings) {
        self.config.turn_mode = settings.mode;
        self.config.min_silence_duration_ms = settings.pause_ms;
    }
    fn trim_trailing_silence(&mut self) {
        let excess = self
            .silence_samples
            .saturating_sub(self.config.speech_pad_ms * 16);
        self.audio.truncate(self.audio.len().saturating_sub(excess));
    }
    pub fn reset(&mut self) {
        self.speaking = false;
        self.speech_samples = 0;
        self.silence_samples = 0;
        self.audio.clear();
        self.padding.clear();
        self.capture_full = false;
    }
}
