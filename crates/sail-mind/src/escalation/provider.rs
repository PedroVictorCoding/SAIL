//! External model providers (Kilo). Spec: the module doc in `escalation/mod.rs`.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use super::ProviderConfig;

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderReply {
    pub text: String,
    /// The model the endpoint says answered (falls back to the configured model).
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderError {
    /// The key variable is unset or empty, or the adapter is unknown.
    Unavailable(String),
    RateLimited {
        retry_after: Option<DateTime<Utc>>,
    },
    Http(String),
    Cancelled,
}

#[async_trait]
pub trait ExternalProvider: Send + Sync {
    fn id(&self) -> &str;
    fn model(&self) -> &str;
    /// `system` is SAIL's fixed instruction; `prompt` is already redacted.
    async fn complete(
        &self,
        system: &str,
        prompt: &str,
        max_tokens: u32,
        cancel: CancellationToken,
    ) -> Result<ProviderReply, ProviderError>;
}

/// `POST {base_url}/chat/completions`, OpenAI-compatible (OpenRouter, OpenAI, gateways).
pub struct OpenAiCompatible {
    pub config: ProviderConfig,
    pub client: reqwest::Client,
}

impl OpenAiCompatible {
    /// Rejects non-https base URLs unless the host is loopback.
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let _ = config;
        unimplemented!("Kilo: escalation::provider::OpenAiCompatible::new")
    }
}

#[async_trait]
impl ExternalProvider for OpenAiCompatible {
    fn id(&self) -> &str {
        &self.config.id
    }
    fn model(&self) -> &str {
        &self.config.model
    }
    async fn complete(
        &self,
        system: &str,
        prompt: &str,
        max_tokens: u32,
        cancel: CancellationToken,
    ) -> Result<ProviderReply, ProviderError> {
        let _ = (system, prompt, max_tokens, cancel);
        unimplemented!("Kilo: escalation::provider::OpenAiCompatible::complete")
    }
}

/// Build the adapter a config names. Unknown adapters → `Unavailable`.
pub fn build(
    config: &ProviderConfig,
) -> Result<std::sync::Arc<dyn ExternalProvider>, ProviderError> {
    match config.adapter.as_str() {
        "openai_compatible" => Ok(std::sync::Arc::new(OpenAiCompatible::new(config.clone())?)),
        other => Err(ProviderError::Unavailable(format!(
            "unknown adapter {other}"
        ))),
    }
}
