use crate::{backend::InferenceBackend, config::LocalModelConfig, validation::validate_request};
use async_trait::async_trait;
use futures_util::StreamExt;
use sail_protocol::*;
use serde_json::{Value, json};
use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::{Mutex as AsyncMutex, mpsc},
};
use tokio_util::sync::CancellationToken;

pub struct LocalGemmaBackend {
    config: LocalModelConfig,
    client: reqwest::Client,
    endpoint: reqwest::Url,
    process: AsyncMutex<Option<Child>>,
    startup: AsyncMutex<()>,
    snapshot: Arc<Mutex<ModelStatus>>,
    parallel: usize,
}
fn unavailable(message: impl Into<String>) -> InferenceError {
    InferenceError::new(ErrorCode::BackendUnavailable, message)
}
fn protocol(message: impl Into<String>) -> InferenceError {
    InferenceError::new(ErrorCode::BackendProtocol, message)
}
impl LocalGemmaBackend {
    pub fn new(config: LocalModelConfig, parallel: usize) -> Result<Self> {
        let endpoint = config.validate()?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.probe_timeout);
        // Do not let a custom DNS resolver redirect localhost outside the host.
        if endpoint.host_str() == Some("localhost") {
            builder = builder.resolve(
                "localhost",
                std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    endpoint.port_or_known_default().unwrap(),
                )),
            );
        }
        let client = builder
            .build()
            .map_err(|e| InferenceError::new(ErrorCode::InvalidConfiguration, e.to_string()))?;
        let active_profile = Some(config.to_profile());
        let snapshot = ModelStatus {
            version: VERSION,
            state: ModelState::Uninitialized,
            model: config.name.clone(),
            capabilities: None,
            requested_backend: config.compute,
            observed_backend: None,
            owned_process: false,
            process_id: None,
            last_error: None,
            active_profile,
            hardware: None,
        };
        Ok(Self {
            config,
            client,
            endpoint,
            process: AsyncMutex::new(None),
            startup: AsyncMutex::new(()),
            snapshot: Arc::new(Mutex::new(snapshot)),
            parallel,
        })
    }
    fn snapshot(&self) -> ModelStatus {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    fn update(&self, update: impl FnOnce(&mut ModelStatus)) {
        update(&mut self.snapshot.lock().unwrap_or_else(|p| p.into_inner()));
    }
    fn url(&self, path: &str) -> reqwest::Url {
        self.endpoint.join(path).expect("static local route")
    }
    async fn json(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        let response = request
            .timeout(self.config.probe_timeout)
            .send()
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        let status = response.status();
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            data.extend_from_slice(&chunk.map_err(|e| unavailable(e.to_string()))?);
            if data.len() > 1_048_576 {
                return Err(protocol("Model metadata exceeded 1MB"));
            }
        }
        if !status.is_success() {
            return Err(unavailable(format!(
                "Local model returned {status}: {}",
                String::from_utf8_lossy(&data)
                    .chars()
                    .take(512)
                    .collect::<String>()
            )));
        }
        serde_json::from_slice(&data).map_err(|e| protocol(e.to_string()))
    }
    async fn probe(&self) -> Result<ModelStatus> {
        self.json(self.client.get(self.url("/health"))).await?;
        let props = self.json(self.client.get(self.url("/props"))).await?;
        let context = props["default_generation_settings"]["n_ctx"]
            .as_u64()
            .or_else(|| props["n_ctx"].as_u64())
            .map(|n| n as usize)
            .unwrap_or(self.config.context_tokens)
            .min(self.config.context_tokens);
        let observed = props["model_path"].as_str().filter(|p| !p.is_empty());
        if let Some(observed) = observed
            && std::path::Path::new(observed).file_name() != self.config.model_path.file_name()
        {
            return Err(InferenceError::new(
                ErrorCode::UnknownModel,
                "Local endpoint is serving a different GGUF; configure the intended model explicitly",
            ));
        }
        let caps = ModelCapabilities {
            model: observed.unwrap_or(&self.config.name).into(),
            text: true,
            vision: props["modalities"]["vision"] == true,
            tools: props["modalities"]["tools"] == true
                || props["chat_template"]
                    .as_str()
                    .is_some_and(|t| t.contains("tools") || t.contains("tool_calls")),
            context_tokens: context,
        };
        let hw = Some(crate::hardware::HardwareProbe::diagnose(&self.config.server_binary).await);
        self.update(|s| {
            s.state = ModelState::Ready;
            s.capabilities = Some(caps);
            s.last_error = None;
            s.hardware = hw;
            s.active_profile = Some(self.config.to_profile());
        });
        Ok(self.snapshot())
    }
    async fn start_inner(&self) -> Result<ModelStatus> {
        let _start = self.startup.lock().await;
        {
            let mut child = self.process.lock().await;
            if child
                .as_mut()
                .is_some_and(|p| p.try_wait().ok().flatten().is_some())
            {
                *child = None;
                self.update(|s| {
                    s.owned_process = false;
                    s.process_id = None;
                    s.observed_backend = None;
                });
            }
        }
        match self.probe().await {
            Ok(status) => return Ok(status),
            Err(error) if error.code == ErrorCode::UnknownModel => return Err(error),
            Err(_) => {}
        }
        self.update(|s| s.state = ModelState::Starting);
        if !self.config.managed {
            return Err(unavailable(
                "Configured external local model service is unavailable",
            ));
        }
        let mut process = self.process.lock().await;
        if process
            .as_mut()
            .is_some_and(|p| p.try_wait().ok().flatten().is_some())
        {
            *process = None;
            self.update(|s| {
                s.owned_process = false;
                s.process_id = None;
                s.observed_backend = None;
            });
        }
        if process.is_none() {
            // A listening but loading service must not be replaced or killed by SAIL.
            let listening = self
                .client
                .get(self.url("/health"))
                .timeout(self.config.probe_timeout)
                .send()
                .await
                .is_ok();
            if !listening {
                if !self.config.model_path.is_file() || !self.config.server_binary.is_file() {
                    return Err(unavailable(
                        "Local Gemma weights or llama-server binary are missing; no download or model substitution is performed",
                    ));
                }
                if self.config.compute == ComputeBackend::Vulkan
                    || self.config.compute == ComputeBackend::Hybrid
                {
                    let render_path = std::path::Path::new("/dev/dri/renderD128");
                    let (accessible, remediation) =
                        crate::hardware::HardwareProbe::check_render_device_access(render_path);
                    if !accessible {
                        let msg =
                            remediation.unwrap_or_else(|| "Render node is inaccessible".into());
                        tracing::warn!(device = %self.config.device, remediation = %msg, "Vulkan/Hybrid device access blocked");
                        return Err(InferenceError::new(
                            ErrorCode::UnsupportedCapability,
                            format!("Requested {} is not available: {}", self.config.device, msg),
                        ));
                    }
                    let output = tokio::time::timeout(
                        self.config.probe_timeout,
                        Command::new(&self.config.server_binary)
                            .arg("--list-devices")
                            .kill_on_drop(true)
                            .output(),
                    )
                    .await
                    .map_err(|_| unavailable("GPU device enumeration timed out"))?
                    .map_err(|e| unavailable(e.to_string()))?;
                    let devices = format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    if !output.status.success()
                        || !devices.lines().any(|line| {
                            line.trim_start()
                                .starts_with(&format!("{}:", self.config.device))
                        })
                    {
                        return Err(InferenceError::new(
                            ErrorCode::UnsupportedCapability,
                            format!(
                                "Requested {} is not available; CPU fallback is not implicit",
                                self.config.device
                            ),
                        ));
                    }
                }
                std::fs::create_dir_all(".local").map_err(|e| unavailable(e.to_string()))?;
                let log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(".local/llama-server.log")
                    .map_err(|e| unavailable(e.to_string()))?;
                let args = self.config.launch_args(self.parallel)?;
                tracing::info!(model=%self.config.name,backend=?self.config.compute,threads=self.config.threads,args=?args,"Starting Rust-supervised local model");
                let mut child = Command::new(&self.config.server_binary)
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| unavailable(e.to_string()))?;
                if let Some(stderr) = child.stderr.take() {
                    tokio::spawn(async move {
                        use std::io::Write;
                        let mut log = log;
                        let mut lines = BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            let _ = writeln!(log, "{line}");
                            tracing::debug!(target:"sail_mind::llama",%line,"Local model output");
                        }
                    });
                }
                self.update(|s| {
                    s.owned_process = true;
                    s.process_id = child.id();
                    s.observed_backend = Some(self.config.compute);
                });
                *process = Some(child);
            }
        }
        drop(process);
        let deadline = Instant::now() + self.config.startup_timeout;
        loop {
            match self.probe().await {
                Ok(status) => return Ok(status),
                Err(error) if error.code == ErrorCode::UnknownModel => return Err(error),
                Err(_) => {}
            }
            let exited = self
                .process
                .lock()
                .await
                .as_mut()
                .is_some_and(|p| p.try_wait().ok().flatten().is_some());
            if exited {
                self.process.lock().await.take();
                self.update(|s| {
                    s.owned_process = false;
                    s.process_id = None;
                    s.observed_backend = None;
                });
                return Err(unavailable(
                    "Owned llama-server exited during model loading; see .local/llama-server.log",
                ));
            }
            if Instant::now() >= deadline {
                // Only kill processes owned by this adapter, never an external service.
                if let Some(mut child) = self.process.lock().await.take() {
                    let _ = child.kill().await;
                }
                self.update(|s| {
                    s.owned_process = false;
                    s.process_id = None;
                    s.observed_backend = None;
                });
                return Err(unavailable("Local model startup timed out"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    async fn check_context(
        &self,
        request: &InferenceRequest,
        caps: &ModelCapabilities,
        images: usize,
    ) -> Result<usize> {
        let mut text_messages = request.messages.clone();
        for message in &mut text_messages {
            if let Content::Parts(parts) = &mut message.content {
                for part in parts {
                    if matches!(part, ContentPart::ImageUrl { .. }) {
                        *part = ContentPart::Text {
                            text: "[authorized image]".into(),
                        };
                    }
                }
            }
        }
        let mut body = json!({"messages":text_messages,"add_generation_prompt":true,"tool_choice":request.tool_choice,"chat_template_kwargs":{"enable_thinking":false}});
        if !request.tools.is_empty() {
            body["tools"] = json!(request.tools);
        }
        let template = self
            .json(self.client.post(self.url("/apply-template")).json(&body))
            .await?;
        let prompt = template["prompt"]
            .as_str()
            .ok_or_else(|| protocol("/apply-template did not return a prompt"))?;
        let tokens = self
            .json(
                self.client
                    .post(self.url("/tokenize"))
                    .json(&json!({"content":prompt,"add_special":false,"parse_special":true})),
            )
            .await?;
        let count = tokens["tokens"]
            .as_array()
            .ok_or_else(|| protocol("/tokenize did not return tokens"))?
            .len();
        if count
            .saturating_add(images * 1280)
            .saturating_add(request.generation.max_tokens as usize)
            > caps.context_tokens
        {
            return Err(InferenceError::new(
                ErrorCode::ResourceLimit,
                "Rendered prompt, image reservation and response headroom exceed the model context window",
            ));
        }
        Ok(count)
    }
    async fn stream(
        &self,
        request: &InferenceRequest,
        events: Option<&mpsc::Sender<InferenceEvent>>,
        limits: &ResourceLimits,
        input_tokens: usize,
    ) -> Result<InferenceResponse> {
        let started = Instant::now();
        let mut body = json!({"model":self.config.name,"messages":request.messages,"stream":true,"stream_options":{"include_usage":true},"max_tokens":request.generation.max_tokens,"temperature":request.generation.temperature,"tool_choice":request.tool_choice,"chat_template_kwargs":{"enable_thinking":false}});
        if !request.tools.is_empty() {
            body["tools"] = json!(request.tools);
        }
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(unavailable(format!(
                "Local generation returned {}",
                response.status()
            )));
        }
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut data = String::new();
        let mut state = StreamState::default();
        while let Some(bytes) = stream.next().await {
            buffer.extend_from_slice(&bytes.map_err(|e| unavailable(e.to_string()))?);
            if buffer.len() > 1_048_576 {
                return Err(protocol("SSE frame exceeded 1MB"));
            }
            while let Some(end) = buffer.iter().position(|&c| c == b'\n') {
                let line: Vec<_> = buffer.drain(..=end).collect();
                let line = std::str::from_utf8(&line)
                    .map_err(|e| protocol(e.to_string()))?
                    .trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    if !data.is_empty() {
                        state
                            .consume(&data, request, events, limits, started)
                            .await?;
                        data.clear();
                    }
                } else if let Some(value) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value.trim_start());
                }
            }
        }
        if !buffer.is_empty() {
            let line = std::str::from_utf8(&buffer).map_err(|e| protocol(e.to_string()))?;
            if let Some(value) = line.strip_prefix("data:") {
                data.push_str(value.trim());
            }
        }
        if !data.is_empty() {
            state
                .consume(&data, request, events, limits, started)
                .await?;
        }
        if !state.finished {
            return Err(protocol("Local model disconnected before a finish reason"));
        }
        if state.reason == "length" && !state.calls.is_empty() {
            return Err(protocol(
                "Tool call was truncated at the output token limit",
            ));
        }
        let calls = state
            .calls
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<ToolCall>, _>>()
            .map_err(|e| protocol(e.to_string()))?;
        for call in &calls {
            if call.id.is_empty()
                || serde_json::from_str::<Value>(&call.function.arguments).is_err()
            {
                return Err(protocol("Malformed completed tool call"));
            }
            if request.tool_choice == ToolChoice::None
                || !request
                    .tools
                    .iter()
                    .any(|t| t.function.name == call.function.name)
            {
                return Err(protocol("Model returned an unrequested function call"));
            }
        }
        Ok(InferenceResponse {
            version: VERSION,
            request_id: request.request_id,
            model: self.config.name.clone(),
            message: Message {
                role: Role::Assistant,
                content: Content::Text(state.content),
                tool_calls: calls,
                tool_call_id: None,
                name: None,
            },
            finish_reason: state.reason,
            usage: Usage {
                input_tokens: Some(state.input_tokens.unwrap_or(input_tokens as u32)),
                output_tokens: state.output_tokens,
            },
            timings: Timings {
                queue_ms: 0,
                first_token_ms: state.first_ms,
                inference_ms: started.elapsed().as_millis() as u64,
            },
        })
    }
}
#[derive(Default)]
struct StreamState {
    content: String,
    calls: Vec<Value>,
    finished: bool,
    reason: String,
    first_ms: Option<u64>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    bytes: usize,
}
impl StreamState {
    async fn consume(
        &mut self,
        data: &str,
        request: &InferenceRequest,
        events: Option<&mpsc::Sender<InferenceEvent>>,
        limits: &ResourceLimits,
        started: Instant,
    ) -> Result<()> {
        if data == "[DONE]" {
            return Ok(());
        }
        let value: Value = serde_json::from_str(data).map_err(|e| protocol(e.to_string()))?;
        if value.get("error").is_some() {
            return Err(protocol(format!("Model stream error: {}", value["error"])));
        }
        if let Some(usage) = value.get("usage") {
            self.input_tokens = usage["prompt_tokens"].as_u64().map(|n| n as u32);
            self.output_tokens = usage["completion_tokens"].as_u64().map(|n| n as u32);
            if self
                .output_tokens
                .is_some_and(|n| n > request.generation.max_tokens)
            {
                return Err(InferenceError::new(
                    ErrorCode::ResourceLimit,
                    "Model exceeded output token budget",
                ));
            }
        }
        let Some(choice) = value["choices"].as_array().and_then(|a| a.first()) else {
            return Ok(());
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finished = true;
            self.reason = reason.into();
        }
        if let Some(text) = choice["delta"]["content"]
            .as_str()
            .filter(|text| !text.is_empty())
        {
            self.bytes += text.len();
            if self.bytes > limits.max_response_bytes {
                return Err(InferenceError::new(
                    ErrorCode::ResourceLimit,
                    "Model exceeded output byte budget",
                ));
            }
            self.first_ms
                .get_or_insert(started.elapsed().as_millis() as u64);
            self.content.push_str(text);
            emit(
                events,
                request.request_id,
                InferenceEventPayload::TextDelta { text: text.into() },
            )
            .await?;
        }
        if let Some(calls) = choice["delta"]["tool_calls"].as_array() {
            for call in calls {
                let index = call["index"]
                    .as_u64()
                    .ok_or_else(|| protocol("Missing tool-call index"))?
                    as usize;
                if index >= 8 {
                    return Err(protocol("Model exceeded tool-call count limit"));
                }
                while self.calls.len() <= index {
                    self.calls.push(
                        json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                    );
                }
                for (key, val) in [
                    ("id", call["id"].as_str()),
                    ("name", call["function"]["name"].as_str()),
                    ("arguments", call["function"]["arguments"].as_str()),
                ] {
                    if let Some(val) = val {
                        self.bytes += val.len();
                        if self.bytes > limits.max_response_bytes {
                            return Err(InferenceError::new(
                                ErrorCode::ResourceLimit,
                                "Model exceeded tool argument byte budget",
                            ));
                        }
                        let target = if key == "id" {
                            &mut self.calls[index][key]
                        } else {
                            &mut self.calls[index]["function"][key]
                        };
                        *target = json!(format!("{}{val}", target.as_str().unwrap_or_default()));
                    }
                }
                self.first_ms
                    .get_or_insert(started.elapsed().as_millis() as u64);
                emit(
                    events,
                    request.request_id,
                    InferenceEventPayload::ToolDelta {
                        index,
                        delta: call.clone(),
                    },
                )
                .await?;
            }
        }
        Ok(())
    }
}
pub(crate) async fn emit(
    events: Option<&mpsc::Sender<InferenceEvent>>,
    id: uuid::Uuid,
    payload: InferenceEventPayload,
) -> Result<()> {
    if let Some(events) = events {
        events
            .send(InferenceEvent {
                version: VERSION,
                request_id: id,
                payload,
            })
            .await
            .map_err(|_| {
                InferenceError::new(
                    ErrorCode::ConsumerClosed,
                    "Inference event consumer disconnected",
                )
            })?;
    }
    Ok(())
}
#[async_trait]
impl InferenceBackend for LocalGemmaBackend {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn aliases(&self) -> Vec<String> {
        let mut aliases = self.config.aliases.clone();
        aliases.push(self.config.model_path.display().to_string());
        aliases
    }
    async fn start(&self) -> Result<ModelStatus> {
        let result = self.start_inner().await;
        if let Err(error) = &result {
            self.update(|s| {
                s.state = ModelState::Unavailable;
                s.capabilities = None;
                s.last_error = Some(error.clone());
            });
        }
        result
    }
    async fn status(&self) -> ModelStatus {
        if self.snapshot().state == ModelState::Stopped {
            return self.snapshot();
        }
        if let Err(error) = self.probe().await {
            self.update(|s| {
                s.state = if self.startup.try_lock().is_err() {
                    ModelState::Starting
                } else {
                    ModelState::Unavailable
                };
                s.capabilities = None;
                s.last_error = Some(error);
            });
        }
        self.snapshot()
    }
    async fn infer(
        &self,
        request: &InferenceRequest,
        events: Option<&mpsc::Sender<InferenceEvent>>,
        cancel: &CancellationToken,
        limits: &ResourceLimits,
    ) -> Result<InferenceResponse> {
        tokio::select! {biased;_ = cancel.cancelled()=>Err(InferenceError::new(ErrorCode::Cancelled,"Inference cancelled")),result=async{
            let images=validate_request(request,limits)?;
            let status=self.start().await?;let caps=status.capabilities.ok_or_else(||unavailable("No model capabilities"))?;
            if images>0&&!caps.vision{return Err(InferenceError::new(ErrorCode::UnsupportedCapability,"Loaded model does not advertise vision support; a local projector may be required"));}
            if !request.tools.is_empty()&&request.tool_choice!=ToolChoice::None&&!caps.tools{return Err(InferenceError::new(ErrorCode::UnsupportedCapability,"Loaded model does not advertise tool support"));}
            let input_tokens=self.check_context(request,&caps,images).await?;
            self.stream(request,events,limits,input_tokens).await
        }=>result}
    }
    async fn shutdown(&self) -> Result<()> {
        let _start = self.startup.lock().await;
        if let Some(mut child) = self.process.lock().await.take()
            && child
                .try_wait()
                .map_err(|e| unavailable(e.to_string()))?
                .is_none()
        {
            child.kill().await.map_err(|e| unavailable(e.to_string()))?;
        }
        self.update(|s| {
            s.state = ModelState::Stopped;
            s.owned_process = false;
            s.process_id = None;
            s.observed_backend = None;
        });
        Ok(())
    }
}
