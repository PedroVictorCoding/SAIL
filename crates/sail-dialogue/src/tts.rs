use sail_common::error::Result;
use sail_mind::VoiceModelServices;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    pub voice_timbre: String,
    pub language: String,
    pub speech_speed: f32,
}
impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            voice_timbre: std::env::var("SAIL_TTS_VOICE").unwrap_or("af_heart".into()),
            language: std::env::var("SAIL_TTS_LANGUAGE").unwrap_or("en-us".into()),
            speech_speed: 1.0,
        }
    }
}
#[derive(Clone)]
pub struct LocalNeuralTts {
    config: TtsConfig,
    services: VoiceModelServices,
    pub threads: usize,
}
impl LocalNeuralTts {
    pub fn new(config: TtsConfig) -> Self {
        Self::with_services(config, VoiceModelServices::new())
    }
    pub fn with_services(config: TtsConfig, services: VoiceModelServices) -> Self {
        Self {
            config,
            services,
            threads: 4,
        }
    }
    pub async fn warm_up(&self) -> Result<()> {
        self.services.warm_tts(self.threads).await
    }
    pub fn set_voice(&mut self, voice: &str, language: &str) {
        self.config.voice_timbre = voice.into();
        self.config.language = language.into();
    }
    pub async fn synthesize_pcm(&self, text: &str) -> Result<(Vec<i16>, u32)> {
        if text.trim().is_empty() {
            return Ok((Vec::new(), 24000));
        }
        self.services
            .synthesize(
                text,
                &self.config.voice_timbre,
                &self.config.language,
                self.config.speech_speed,
                self.threads,
            )
            .await
    }
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        let (pcm, rate) = self.synthesize_pcm(text).await?;
        Ok(crate::stt_wav_encode(&pcm, rate))
    }
}
