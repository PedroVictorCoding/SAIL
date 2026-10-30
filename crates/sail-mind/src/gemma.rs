use reqwest::header::CONTENT_TYPE;
use sail_common::diagnostics::log_json;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;
use tracing::{debug, error, trace};
use tracing::{info, warn};

use sail_common::error::Result;

//! Compatibility facade. All inference uses Mind's shared typed service and budgets.
use crate::{config::LocalModelConfig, service::MindService};
use sail_common::error::{Result, SailError};
use sail_protocol::{InferenceEventPayload, InferenceRequest, Message, Role, ToolChoice};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GemmaConfig {
    pub model_path: PathBuf,
    pub server_url: String,
    pub llama_server_binary: PathBuf,
    pub threads: usize,
    pub ctx_size: usize,
    pub port: u16,
    pub mmproj_path: Option<PathBuf>,
}
impl Default for GemmaConfig {
    fn default() -> Self {
        let config = LocalModelConfig::default();
        let port = reqwest::Url::parse(&config.endpoint)
            .ok()
            .and_then(|u| u.port_or_known_default())
            .unwrap_or(8081);
        Self {
            model_path: config.model_path,
            server_url: config.endpoint,
            llama_server_binary: config.server_binary,
            threads: config.threads,
            ctx_size: config.context_tokens,
            port,
            mmproj_path: config.projector,
        }
    }
}
#[derive(Clone)]
pub struct GemmaRunner {
    service: sail_protocol::Result<MindService>,
}
fn legacy(error: sail_protocol::InferenceError) -> SailError {
    SailError::Inference(error.to_string())
}

#[derive(Serialize, Deserialize, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

fn forward_model_output(
    reader: impl Read + Send + 'static,
    log: Arc<Mutex<std::fs::File>>,
    source: &'static str,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(line) => {
                    if let Ok(mut file) = log.lock() {
                        let _ = writeln!(file, "[{source}] {line}");
                    }
                    debug!(target:"sail_mind::llama",source,line=%line,"Local llama-server output");
                }
                Err(error) => {
                    warn!(%error,source,"Failed to read llama-server output");
                    break;
                }
            }
        }
    });
}

impl GemmaRunner {
    pub fn new(config: GemmaConfig) -> Self {
        let service = LocalModelConfig::from_env().and_then(|mut local| {
            local.endpoint = config.server_url;
            local.model_path = config.model_path;
            local.server_binary = config.llama_server_binary;
            local.projector = config.mmproj_path;
            local.threads = config.threads;
            local.context_tokens = config.ctx_size;
            local.managed = local.managed && local.model_path.is_file();
            MindService::new(local, Default::default())
        });
        Self { service }
    }
    pub fn from_service(service: MindService) -> Self {
        Self {
            service: Ok(service),
        }
    }
    pub fn service(&self) -> sail_protocol::Result<MindService> {
        self.service.clone()
    }
    pub async fn ensure_server_running(&mut self) -> Result<bool> {
        let startup = Instant::now();
        let health_url = format!("http://127.0.0.1:{}/health", self.config.port);
        if let Ok(res) = self
            .client
            .get(&health_url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            && res.status().is_success()
        {
            info!(
                "Existing llama-server with Gemma 4 is already responsive on port {}",
                self.config.port
            );
            return Ok(true);
        }

        if !self.config.llama_server_binary.exists() || !self.config.model_path.exists() {
            warn!(
                binary = ?self.config.llama_server_binary,
                model = ?self.config.model_path,
                "llama-server binary or Gemma 4 weights not accessible on disk; inference unavailable"
            );
            return Ok(false);
        }

        info!(
            model = ?self.config.model_path,
            threads = self.config.threads,
            port = self.config.port,
            "Spawning local llama-server for Gemma 4"
        );

        let mut command = Command::new(&self.config.llama_server_binary);
        if let Some(projector) = &self.config.mmproj_path
            && projector.exists()
        {
            command.arg("--mmproj").arg(projector);
        }
        std::fs::create_dir_all(".local")?;
        let log = Arc::new(Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(".local/llama-server.log")?,
        ));
        match command
            .arg("-m")
            .arg(&self.config.model_path)
            .arg("--jinja")
            .arg("-t")
            .arg(self.config.threads.to_string())
            .arg("-c")
            .arg(self.config.ctx_size.to_string())
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(self.config.port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                info!(pid=child.id(),binary=%self.config.llama_server_binary.display(),projector=?self.config.mmproj_path,context_tokens=self.config.ctx_size,log_path=".local/llama-server.log","Local model process spawned; forwarding its output to runner logs");
                if let Some(stdout) = child.stdout.take() {
                    forward_model_output(stdout, log.clone(), "stdout");
                }
                if let Some(stderr) = child.stderr.take() {
                    forward_model_output(stderr, log, "stderr");
                }
                self.process = Some(child);
                info!("Spawned llama-server process; waiting for warm-up");
                for attempt in 0..240 {
                    if attempt % 10 == 0 {
                        debug!(attempt,elapsed_ms=startup.elapsed().as_millis(),%health_url,"Waiting for local model readiness");
                    }
                    if self
                        .process
                        .as_mut()
                        .is_some_and(|p| p.try_wait().ok().flatten().is_some())
                    {
                        return Err(sail_common::error::SailError::Inference(
                            "llama-server exited during startup".into(),
                        ));
                    }
                    if self
                        .client
                        .get(&health_url)
                        .timeout(Duration::from_secs(1))
                        .send()
                        .await
                        .is_ok_and(|r| r.status().is_success())
                    {
                        info!(
                            elapsed_ms = startup.elapsed().as_millis(),
                            "Local Gemma server is ready for inference"
                        );
                        return Ok(true);
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(sail_common::error::SailError::Inference(
                    "llama-server startup timed out".into(),
                ))
            }
            Err(err) => {
                warn!(error = %err, "Failed to spawn llama-server process; inference unavailable");
                Ok(false)
            }
        }
        self.service()
            .map_err(legacy)?
            .start()
            .await
            .map_err(legacy)?;
        Ok(true)
    }
    pub async fn complete(&self, prompt: &str) -> Result<String> {
        let url = reqwest::Url::parse(&self.config.server_url)
            .map_err(|e| sail_common::error::SailError::Config(e.to_string()))?;
        if !matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
        ) {
            return Err(sail_common::error::SailError::Config(
                "Inference must use a loopback endpoint".into(),
            ));
        }
        let started = Instant::now();
        info!(endpoint=%self.config.server_url,prompt_chars=prompt.chars().count(),"Starting non-streaming local inference");
        debug!(prompt, "Local inference prompt");
        let req = ChatCompletionRequest {
            model: "gemma-4".to_string(),
        let request = InferenceRequest {
            messages: vec![
                Message::text(Role::System, "You are SAIL, a concise local assistant."),
                Message::text(Role::User, prompt),
            ],
            ..InferenceRequest::text(prompt)
        };
        let response = self
            .service()
            .map_err(legacy)?
            .infer(request, CancellationToken::new(), None)
            .await
            && res.status().is_success()
            && let Ok(chat_res) = res.json::<ChatCompletionResponse>().await
            && let Some(choice) = chat_res.choices.into_iter().next()
        {
            info!(
                elapsed_ms = started.elapsed().as_millis(),
                response_chars = choice.message.content.chars().count(),
                "Local inference completed"
            );
            debug!(response=%choice.message.content,"Local inference response");
            return Ok(choice.message.content);
        }

        Err(sail_common::error::SailError::Inference(
            "Local Gemma inference unavailable; check llama-server and model weights".into(),
        ))
    }
}

impl Drop for GemmaRunner {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            info!(pid = child.id(), "Stopping SAIL-owned llama-server process");
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl GemmaRunner {
    pub async fn health(&self) -> serde_json::Value {
        let Ok(url) = reqwest::Url::parse(&self.config.server_url) else {
            return serde_json::json!({"ready":false,"error":"Invalid model URL"});
        };
        if !matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
        ) {
            return serde_json::json!({"ready":false,"error":"Model URL must be loopback"});
        }
        let base = format!("{}://{}", url.scheme(), url.authority());
        match self
            .client
            .get(format!("{base}/props"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match response.json::<serde_json::Value>().await {
                    Ok(props) => {
                        serde_json::json!({"ready":true,"model":props["model_path"],"modalities":props["modalities"]})
                    }
                    Err(e) => serde_json::json!({"ready":false,"error":e.to_string()}),
                }
            .map_err(legacy)?;
        Ok(response.message.content.text().unwrap_or_default().into())
    }
    pub async fn health(&self) -> Value {
        match self.service() {
            Err(error) => json!({"ready":false,"error":error}),
            Ok(service) => {
                let status = service.status().await;
                let model = status.models.first();
                json!({"ready":model.is_some_and(|s|s.state==sail_protocol::ModelState::Ready),"model":model.map(|s|&s.model),"modalities":{"vision":model.and_then(|s|s.capabilities.as_ref()).is_some_and(|c|c.vision),"text":model.and_then(|s|s.capabilities.as_ref()).is_some_and(|c|c.text)},"mind":status})
            }
        }
    }
}

impl GemmaRunner {
    /// Stream a local OpenAI-compatible response, retaining tool calls for Agents authorization.
    #[tracing::instrument(name="gemma_stream",skip_all,fields(request_id=%uuid::Uuid::new_v4()))]
    pub async fn stream_chat(
        &self,
        messages: Vec<Value>,
        tools: Value,
        tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<serde_json::Value> {
        use futures_util::StreamExt;
        use sail_common::error::SailError;
        let url = reqwest::Url::parse(&self.config.server_url)
            .map_err(|e| SailError::Config(e.to_string()))?;
        if !matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
        ) {
            return Err(SailError::Config(
                "Voice inference must use a loopback model endpoint".into(),
            ));
        }
        let started = Instant::now();
        let tool_count = tools.as_array().map_or(0, Vec::len);
        info!(endpoint=%self.config.server_url,messages=messages.len(),tool_count,"Submitting local streamed model request");
        debug!(messages=%log_json(&serde_json::json!(messages),8192),tools=%log_json(&tools,4096),"Prepared local model request contents");
        let mut body = serde_json::json!({"model":"local","messages":messages,"stream":true,"temperature":0.4,"max_tokens":384,"chat_template_kwargs":{"enable_thinking":false}});
        if tools.as_array().is_some_and(|t| !t.is_empty()) {
            body["tools"] = tools;
            body["tool_choice"] = serde_json::json!("auto");
        } else {
            body["tool_choice"] = serde_json::json!("none");
        }
        let response = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SailError::Inference(format!("Local Gemma unavailable: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!(%status,response=%log_json(&serde_json::json!(body),2048),elapsed_ms=started.elapsed().as_millis(),"Local model rejected inference request");
            return Err(SailError::Inference(format!(
                "Local Gemma {status}: {}",
                body.chars().take(1000).collect::<String>()
            )));
        }
        debug!(status=%response.status(),header_latency_ms=started.elapsed().as_millis(),"Model HTTP response accepted; reading SSE stream");
        let mut deltas = 0usize;
        let mut first_token = false;
        let mut finish_reason = String::new();
        let mut stream = response.bytes_stream();
        let mut pending = Vec::new();
        let mut content = String::new();
        let mut calls: Vec<serde_json::Value> = Vec::new();
        let mut finished = false;
        while let Some(bytes) = stream.next().await {
            pending.extend_from_slice(&bytes.map_err(|e| SailError::Inference(e.to_string()))?);
            if pending.len() > 1_048_576 {
                return Err(SailError::Inference(
                    "Model stream exceeded frame limit".into(),
                ));
            }
            while let Some(end) = pending.iter().position(|&c| c == b'\n') {
                let line: Vec<_> = pending.drain(..=end).collect();
                let line = std::str::from_utf8(&line)
                    .map_err(|e| SailError::Inference(e.to_string()))?
                    .trim();
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                if data == "[DONE]" {
                    finished = true;
                    break;
                }
                let event: serde_json::Value = serde_json::from_str(data)?;
                let choice = &event["choices"][0];
                if let Some(reason) = choice["finish_reason"].as_str() {
                    finish_reason = reason.into();
                    finished = true;
                }
                let delta = &choice["delta"];
                if let Some(text) = delta["content"].as_str() {
                    if !first_token {
                        first_token = true;
                        info!(
                            ttft_ms = started.elapsed().as_millis(),
                            "First response text received from Gemma"
                        );
                    }
                    deltas += 1;
                    trace!(delta = deltas, text, "Model text delta");
                    content.push_str(text);
                    let _ = tx.send(text.into()).await;
                }
                if let Some(tool_calls) = delta["tool_calls"].as_array() {
                    trace!(tool_deltas=%log_json(&serde_json::json!(tool_calls),4096),"Model function-call delta");
                    for call in tool_calls {
                        let index = call["index"].as_u64().unwrap_or(0) as usize;
                        if index >= 8 {
                            return Err(SailError::Inference("Too many model tool calls".into()));
                        }
                        while calls.len() <= index {
                            calls.push(serde_json::json!({"id":"","type":"function","function":{"name":"","arguments":""}}));
                        }
                        for (path, value) in [
                            ("id", call["id"].as_str()),
                            ("name", call["function"]["name"].as_str()),
                            ("arguments", call["function"]["arguments"].as_str()),
                        ] {
                            if let Some(v) = value {
                                let target = if path == "id" {
                                    &mut calls[index][path]
                                } else {
                                    &mut calls[index]["function"][path]
                                };
                                *target = serde_json::Value::String(format!(
                                    "{}{v}",
                                    target.as_str().unwrap_or_default()
                                ));
                            }
                        }
                    }
                }
            }
        }
        if !finished {
            return Err(SailError::Inference(
                "Local model stream ended before completion".into(),
            ));
        }
        info!(elapsed_ms=started.elapsed().as_millis(),deltas,response_chars=content.chars().count(),tool_calls=calls.len(),%finish_reason,"Gemma stream completed");
        debug!(response=%content,tool_calls=%log_json(&serde_json::json!(calls),4096),"Final model response and proposed operations");
        let mut result = serde_json::json!({"role":"assistant","content":content});
        if !calls.is_empty() {
            result["tool_calls"] = calls.into();
        }
        Ok(result)
    ) -> Result<Value> {
        let service = self.service().map_err(legacy)?;
        let messages = messages
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let tools = serde_json::from_value(tools)?;
        let mut request = InferenceRequest {
            messages,
            tools,
            ..InferenceRequest::text("")
        };
        request.tool_choice = if request.tools.is_empty() {
            ToolChoice::None
        } else {
            ToolChoice::Auto
        };
        let (events, mut rx) = tokio::sync::mpsc::channel::<sail_protocol::InferenceEvent>(32);
        let forward = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let InferenceEventPayload::TextDelta { text } = event.payload
                    && tx.send(text).await.is_err()
                {
                    break;
                }
            }
        });
        struct Forward(tokio::task::JoinHandle<()>);
        impl Drop for Forward {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let mut forward = Forward(forward);
        let response = service
            .infer(request, CancellationToken::new(), Some(events))
            .await
            .map_err(legacy)?;
        let _ = (&mut forward.0).await;
        Ok(serde_json::to_value(response.message)?)
    }
}
