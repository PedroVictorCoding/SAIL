//! External model providers (Kilo). Spec: the module doc in `escalation/mod.rs`.
use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use tokio_util::sync::CancellationToken;

use super::ProviderConfig;

/// Timeout for one provider call. The policy may wait at most [`super::service::MAX_WAIT`];
/// the rest of the answer arrives through the background job.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest provider error body kept in an error message.
const MAX_ERROR_BODY: usize = 300;

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
        let url = Url::parse(&config.base_url)
            .map_err(|error| ProviderError::Unavailable(format!("invalid base_url: {error}")))?;
        let loopback = url.host_str().is_some_and(is_loopback_host);
        match url.scheme() {
            "https" => {}
            "http" if loopback => {}
            other => {
                return Err(ProviderError::Unavailable(format!(
                    "base_url scheme {other} is not allowed; use https (http only for loopback)"
                )));
            }
        }
        if url.host().is_none() {
            return Err(ProviderError::Unavailable(
                "base_url has no host".to_string(),
            ));
        }
        if config.api_key_env.trim().is_empty() {
            return Err(ProviderError::Unavailable(
                "api_key_env is empty".to_string(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ProviderError::Unavailable(format!("http client: {error}")))?;
        Ok(Self { config, client })
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
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
        let key = std::env::var(&self.config.api_key_env)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                ProviderError::Unavailable(format!(
                    "{} is not set; provider {} is unavailable",
                    self.config.api_key_env, self.config.id
                ))
            })?;
        let endpoint = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": prompt},
            ],
            "max_tokens": max_tokens,
        });
        let request = self
            .client
            .post(&endpoint)
            .header(CONTENT_TYPE, "application/json")
            .bearer_auth(key)
            .json(&body);
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            result = request.send() => result.map_err(|error| {
                ProviderError::Http(format!("request to {} failed: {error}", self.config.id))
            })?,
        };
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after);
        let body = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            result = response.text() => result.map_err(|error| {
                ProviderError::Http(format!("reply from {} failed: {error}", self.config.id))
            })?,
        };
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(ProviderError::RateLimited { retry_after });
        }
        if !status.is_success() {
            if mentions_rate_limit(&body) {
                return Err(ProviderError::RateLimited { retry_after });
            }
            return Err(ProviderError::Http(format!(
                "HTTP {}: {}",
                status.as_u16(),
                snippet(&body)
            )));
        }
        parse_reply(&body, &self.config.model)
    }
}

fn parse_reply(body: &str, configured_model: &str) -> Result<ProviderReply, ProviderError> {
    if mentions_rate_limit(body) && body.to_ascii_lowercase().contains("\"error\"") {
        return Err(ProviderError::RateLimited { retry_after: None });
    }
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| ProviderError::Http(format!("invalid JSON from provider: {error}")))?;
    let message = &value["choices"][0]["message"]["content"];
    let Some(text) = message.as_str() else {
        return Err(ProviderError::Http(
            "provider reply has no text content".to_string(),
        ));
    };
    let model = value["model"]
        .as_str()
        .filter(|model| !model.is_empty())
        .unwrap_or(configured_model)
        .to_owned();
    Ok(ProviderReply {
        text: text.to_owned(),
        model,
        input_tokens: value["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: value["usage"]["completion_tokens"].as_u64().unwrap_or(0),
    })
}

fn mentions_rate_limit(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("rate-limit")
        || lower.contains("ratelimit")
        || lower.contains("too many requests")
}

fn parse_retry_after(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<i64>() {
        let seconds = seconds.clamp(0, 86_400);
        return Some(Utc::now() + chrono::Duration::seconds(seconds));
    }
    DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

fn snippet(body: &str) -> String {
    let mut end = body.len().min(MAX_ERROR_BODY);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].replace(['\n', '\r'], " ")
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
