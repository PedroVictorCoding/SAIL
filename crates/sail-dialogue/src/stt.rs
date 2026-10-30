use sail_common::error::Result;
use sail_mind::VoiceModelServices;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    pub sample_rate: u32,
    pub chunk_size_ms: usize,
}
impl Default for SttConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            chunk_size_ms: 800,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingToken {
    pub token: String,
    pub is_partial: bool,
    pub confidence: f32,
    pub timestamp_ms: u64,
}
#[derive(Clone)]
pub struct LocalStreamingStt {
    services: VoiceModelServices,
    pub threads: usize,
}
impl LocalStreamingStt {
    pub fn new(_config: SttConfig) -> Self {
        Self::with_services(VoiceModelServices::new())
    }
    pub fn with_services(services: VoiceModelServices) -> Self {
        Self {
            services,
            threads: 4,
        }
    }
    pub async fn transcribe(
        &self,
        samples: &[i16],
        partial: bool,
    ) -> Result<(String, Vec<StreamingToken>)> {
        if samples.is_empty() {
            return Ok((String::new(), Vec::new()));
        }
        let value = self
            .services
            .transcribe(samples, partial, self.threads)
            .await?;
        Ok((
            value["text"].as_str().unwrap_or_default().into(),
            serde_json::from_value(value["tokens"].clone())?,
        ))
    }
    pub async fn finalize(&self, samples: &[i16]) -> Result<String> {
        Ok(self.transcribe(samples, false).await?.0)
    }
}
