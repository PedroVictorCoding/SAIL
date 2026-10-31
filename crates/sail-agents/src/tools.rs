//! Trusted tool registration and bounded execution. Returned text is data, never policy.
use async_trait::async_trait;
use sail_common::types::SubsystemId;
use sail_guardian::{Capability, GuardianExecutionBroker, ToolCapability};
use sail_protocol::execution::{ToolDescriptor, ToolEffect, ToolEvent, ToolObservation, ToolState};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Semaphore, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ToolFailure {
    pub message: String,
    pub retryable: bool,
}
#[derive(Clone)]
pub struct ToolContext {
    pub request_id: Uuid,
    pub correlation_id: Uuid,
    pub actor: SubsystemId,
    pub cancel: CancellationToken,
}
#[async_trait]
pub trait Tool: Send + Sync {
    /// Trusted adapters may bind a stronger capability than the generic invocation.
    fn capability(&self, tool: &str, input: &Value) -> Capability {
        ToolScheduler::capability(tool, input)
    }
    async fn execute(&self, context: ToolContext, input: Value) -> Result<Value, ToolFailure>;
}
struct Entry {
    descriptor: ToolDescriptor,
    backend: Arc<dyn Tool>,
    slots: Arc<Semaphore>,
}
#[derive(Default)]
pub struct ToolRegistry {
    entries: RwLock<BTreeMap<String, Arc<Entry>>>,
}
impl ToolRegistry {
    pub fn register(
        &self,
        descriptor: ToolDescriptor,
        backend: Arc<dyn Tool>,
    ) -> Result<(), String> {
        if descriptor.id.is_empty()
            || descriptor.id.len() > 96
            || !descriptor
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b":._-".contains(&byte))
            || descriptor.version != 1
            || descriptor.description.len() > 1024
            || descriptor.concurrency_limit == 0
            || descriptor.concurrency_limit > 16
            || descriptor.required_capabilities != vec![descriptor.id.clone()]
            || !["telemetry", "io", "network", "cpu", "sandbox"]
                .contains(&descriptor.resource_class.as_str())
            || descriptor.timeout_ms == 0
            || descriptor.timeout_ms > 120_000
            || descriptor.resource_class.is_empty()
        {
            return Err("invalid tool descriptor".into());
        }
        schema_supported(&descriptor.input_schema, 0)?;
        schema_supported(&descriptor.output_schema, 0)?;
        if serde_json::to_vec(&descriptor)
            .map_err(|e| e.to_string())?
            .len()
            > 8192
        {
            return Err("schema budget exceeded".into());
        }
        let mut entries = self.entries.write().unwrap();
        if entries.contains_key(&descriptor.id) {
            return Err("tool already registered".into());
        }
        entries.insert(
            descriptor.id.clone(),
            Arc::new(Entry {
                slots: Arc::new(Semaphore::new(descriptor.concurrency_limit)),
                descriptor,
                backend,
            }),
        );
        Ok(())
    }
    /// Lexical fast path, no model call. Only available, registered adapters appear.
    pub fn select(&self, query: &str, max_tools: usize, max_bytes: usize) -> Vec<ToolDescriptor> {
        let words: Vec<_> = query
            .split_whitespace()
            .take(32)
            .map(str::to_lowercase)
            .collect();
        let entries = self.entries.read().unwrap();
        let mut ranked: Vec<_> = entries
            .values()
            .map(|entry| {
                let text = format!("{} {}", entry.descriptor.id, entry.descriptor.description)
                    .to_lowercase();
                (
                    words
                        .iter()
                        .filter(|word| text.contains(word.as_str()))
                        .count(),
                    entry,
                )
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        ranked.sort_by(|(a, left), (b, right)| {
            b.cmp(a).then(left.descriptor.id.cmp(&right.descriptor.id))
        });
        let mut bytes = 2;
        let mut result = Vec::new();
        for (_, entry) in ranked {
            let size = serde_json::to_vec(&entry.descriptor).unwrap().len();
            if bytes + size + 1 <= max_bytes.min(16384) && result.len() < max_tools.min(8) {
                bytes += size + 1;
                result.push(entry.descriptor.clone());
            }
        }
        result
    }
}
// An intentionally bounded JSON Schema subset. Unsupported validation keywords
// fail registration rather than silently claiming a full JSON Schema validator.
fn schema_supported(schema: &Value, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err("schema depth".into());
    }
    let object = schema.as_object().ok_or("schema must be object")?;
    let allowed = [
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "maxItems",
        "maxLength",
        "minimum",
        "maximum",
        "enum",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unsupported schema keyword".into());
    }
    if let Some(values) = schema.get("enum") {
        if values
            .as_array()
            .is_none_or(|items| items.is_empty() || items.len() > 32)
        {
            return Err("enum bound".into());
        }
    }
    match schema["type"].as_str() {
        Some("object") => {
            if schema["additionalProperties"] != false {
                return Err("object must deny additional properties".into());
            }
            let properties = schema["properties"]
                .as_object()
                .ok_or("object properties required")?;
            if properties.len() > 32 {
                return Err("property limit".into());
            }
            for child in properties.values() {
                schema_supported(child, depth + 1)?;
            }
            if let Some(required) = schema.get("required") {
                for field in required.as_array().ok_or("required must be array")? {
                    if !properties
                        .contains_key(field.as_str().ok_or("required field must be string")?)
                    {
                        return Err("required field undefined".into());
                    }
                }
            }
        }
        Some("string") => {
            if schema["maxLength"].as_u64().is_none_or(|n| n > 1048576) {
                return Err("string maxLength required".into());
            }
        }
        Some("array") => {
            if schema["maxItems"].as_u64().is_none_or(|n| n > 128) {
                return Err("array maxItems required".into());
            }
            schema_supported(&schema["items"], depth + 1)?;
        }
        Some("integer") => {
            for key in ["minimum", "maximum"] {
                if schema.get(key).is_some_and(|n| n.as_i64().is_none()) {
                    return Err("integer bound required".into());
                }
            }
        }
        Some("boolean" | "null") => {}
        _ => return Err("unsupported schema type".into()),
    }
    Ok(())
}
fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    if schema
        .get("enum")
        .is_some_and(|items| !items.as_array().unwrap().contains(value))
    {
        return Err("enum mismatch".into());
    }
    match schema["type"].as_str().unwrap() {
        "object" => {
            let object = value.as_object().ok_or("object expected")?;
            let properties = schema["properties"].as_object().unwrap();
            if object.keys().any(|key| !properties.contains_key(key)) {
                return Err("unknown input field".into());
            }
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                if required
                    .iter()
                    .any(|field| !object.contains_key(field.as_str().unwrap()))
                {
                    return Err("required field missing".into());
                }
            }
            for (key, value) in object {
                validate(&properties[key], value)?;
            }
        }
        "string" => {
            if value.as_str().is_none_or(|text| {
                text.chars().count() > schema["maxLength"].as_u64().unwrap() as usize
            }) {
                return Err("string bound/type".into());
            }
        }
        "integer" => {
            let number = value.as_i64().ok_or("integer expected")?;
            if schema["minimum"].as_i64().is_some_and(|min| number < min)
                || schema["maximum"].as_i64().is_some_and(|max| number > max)
            {
                return Err("integer range".into());
            }
        }
        "array" => {
            let items = value.as_array().ok_or("array expected")?;
            if items.len() > schema["maxItems"].as_u64().unwrap() as usize {
                return Err("array limit".into());
            }
            for value in items {
                validate(&schema["items"], value)?;
            }
        }
        "boolean" => {
            if !value.is_boolean() {
                return Err("boolean expected".into());
            }
        }
        "null" => {
            if !value.is_null() {
                return Err("null expected".into());
            }
        }
        _ => return Err("unsupported schema".into()),
    }
    Ok(())
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct CachedRun {
    key: String,
    result: Option<ToolObservation>,
    expires: u64,
}
fn failed(context: &ToolContext, tool: &str, error: &str) -> ToolObservation {
    ToolObservation {
        version: 1,
        id: Uuid::new_v4(),
        request_id: context.request_id,
        correlation_id: context.correlation_id,
        source: tool.into(),
        created_at_ms: now_ms(),
        expires_at_ms: now_ms() + 300000,
        success: false,
        data: Value::Null,
        evidence_refs: Vec::new(),
        error: Some(error.into()),
    }
}
struct Payload {
    id: String,
    correlation: Uuid,
    actor: SubsystemId,
    expires: u64,
    data: Vec<u8>,
}
#[derive(Default)]
struct ContentStore {
    items: VecDeque<Payload>,
    bytes: usize,
}
impl ContentStore {
    fn put(&mut self, correlation: Uuid, actor: SubsystemId, data: Vec<u8>, now: u64) -> String {
        while self.items.front().is_some_and(|item| item.expires <= now)
            || self.bytes + data.len() > 8 * 1024 * 1024
        {
            if let Some(old) = self.items.pop_front() {
                self.bytes -= old.data.len();
            } else {
                break;
            }
        }
        let id = format!("observation:{}", Uuid::new_v4());
        self.bytes += data.len();
        self.items.push_back(Payload {
            id: id.clone(),
            correlation,
            actor,
            expires: now + 300000,
            data,
        });
        id
    }
}
pub struct ToolScheduler {
    registry: Arc<ToolRegistry>,
    guardian: Arc<GuardianExecutionBroker>,
    slots: Arc<Semaphore>,
    background: Arc<Semaphore>,
    writes: Arc<Semaphore>,
    events: broadcast::Sender<ToolEvent>,
    content: Mutex<ContentStore>,
    classes: Mutex<HashMap<String, Arc<Semaphore>>>,
    runs: Mutex<HashMap<Uuid, CachedRun>>,
}
impl ToolScheduler {
    pub fn new(
        registry: Arc<ToolRegistry>,
        guardian: Arc<GuardianExecutionBroker>,
        concurrency: usize,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let concurrency = concurrency.clamp(2, 16);
        Self {
            registry,
            guardian,
            slots: Arc::new(Semaphore::new(concurrency)),
            background: Arc::new(Semaphore::new(concurrency - 1)),
            writes: Arc::new(Semaphore::new(1)),
            events,
            content: Mutex::new(ContentStore::default()),
            classes: Mutex::new(HashMap::new()),
            runs: Mutex::new(HashMap::new()),
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<ToolEvent> {
        self.events.subscribe()
    }
    pub fn payload(&self, id: &str, context: &ToolContext) -> Option<Value> {
        let now = now_ms();
        let content = self.content.lock().unwrap();
        let item = content.items.iter().find(|item| {
            item.id == id
                && item.correlation == context.correlation_id
                && item.actor == context.actor
                && item.expires > now
        })?;
        serde_json::from_slice(&item.data).ok()
    }
    pub fn capability(tool: &str, input: &Value) -> Capability {
        Capability::Tool(ToolCapability::Invocation {
            tool: tool.into(),
            arguments: input.to_string(),
        })
    }
    /// Validate the whole dependency graph before dispatching any effect.
    pub async fn execute_plan(
        self: Arc<Self>,
        context: ToolContext,
        steps: Vec<sail_protocol::execution::ToolStep>,
    ) -> Result<Vec<ToolObservation>, String> {
        if steps.len() > 32 {
            return Err("plan step limit".into());
        }
        let order: Vec<_> = steps.iter().map(|step| step.request_id).collect();
        let mut pending: BTreeMap<_, _> = steps
            .into_iter()
            .map(|step| (step.request_id, step))
            .collect();
        if pending.len() != order.len()
            || pending
                .values()
                .any(|step| step.depends_on.iter().any(|id| !pending.contains_key(id)))
        {
            return Err("duplicate step or missing dependency".into());
        }
        let mut checked = std::collections::HashSet::new();
        loop {
            let ready: Vec<_> = pending
                .values()
                .filter(|step| {
                    !checked.contains(&step.request_id)
                        && step.depends_on.iter().all(|id| checked.contains(id))
                })
                .map(|step| step.request_id)
                .collect();
            if ready.is_empty() {
                break;
            }
            checked.extend(ready);
        }
        if checked.len() != pending.len() {
            return Err("cyclic tool plan".into());
        }
        let mut done = HashMap::<Uuid, ToolObservation>::new();
        let mut running = tokio::task::JoinSet::new();
        while !pending.is_empty() || !running.is_empty() {
            let ready: Vec<_> = pending
                .values()
                .filter(|step| step.depends_on.iter().all(|id| done.contains_key(id)))
                .map(|step| step.request_id)
                .collect();
            for id in ready {
                let step = pending.remove(&id).unwrap();
                let child = ToolContext {
                    request_id: id,
                    cancel: context.cancel.child_token(),
                    ..context.clone()
                };
                if step.depends_on.iter().any(|id| !done[id].success) {
                    done.insert(id, failed(&child, &step.tool, "dependency_failed"));
                    continue;
                }
                let scheduler = self.clone();
                running.spawn(async move {
                    let observation = scheduler
                        .execute(
                            child,
                            &step.tool,
                            step.input,
                            step.approval_token,
                            step.foreground,
                        )
                        .await;
                    (id, observation)
                });
            }
            if let Some(result) = running.join_next().await {
                let (id, observation) = result.map_err(|error| error.to_string())?;
                done.insert(id, observation);
            }
        }
        Ok(order
            .into_iter()
            .map(|id| done.remove(&id).unwrap())
            .collect())
    }
    pub async fn execute(
        &self,
        context: ToolContext,
        tool: &str,
        input: Value,
        token: Option<Uuid>,
        foreground: bool,
    ) -> ToolObservation {
        let bytes = match bounded_json(&input, 32768) {
            Ok(bytes) => bytes,
            Err(_) => return failed(&context, tool, "tool_input_limit"),
        };
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let digest: String = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let key = format!(
            "{:?}:{}:{}:{}",
            context.actor, context.correlation_id, tool, digest
        );
        {
            let mut runs = self.runs.lock().unwrap();
            runs.retain(|_, run| run.expires > now_ms());
            if let Some(run) = runs.get(&context.request_id) {
                return if run.key == key {
                    run.result
                        .clone()
                        .unwrap_or_else(|| failed(&context, tool, "duplicate_inflight"))
                } else {
                    failed(&context, tool, "request_id_conflict")
                };
            }
            if runs.len() >= 4096 {
                return failed(&context, tool, "scheduler_capacity");
            }
            runs.insert(
                context.request_id,
                CachedRun {
                    key,
                    result: None,
                    expires: now_ms() + 300000,
                },
            );
        }
        let context = ToolContext {
            cancel: context.cancel.child_token(),
            ..context
        };
        let _cancel_on_drop = CancelOnDrop(context.cancel.clone());
        let result = self
            .execute_once(context.clone(), tool, input, token, foreground)
            .await;
        if let Some(run) = self.runs.lock().unwrap().get_mut(&context.request_id) {
            run.result = Some(result.clone());
        }
        result
    }
    async fn execute_once(
        &self,
        context: ToolContext,
        tool: &str,
        input: Value,
        token: Option<Uuid>,
        foreground: bool,
    ) -> ToolObservation {
        let mut sequence = 0;
        let mut event = |state, approval_session| {
            sequence += 1;
            let _ = self.events.send(ToolEvent {
                version: 1,
                request_id: context.request_id,
                correlation_id: context.correlation_id,
                sequence,
                tool: tool.into(),
                state,
                approval_session,
            });
        };
        event(ToolState::Queued, None);
        let entry = self.registry.entries.read().unwrap().get(tool).cloned();
        let mut observation = ToolObservation {
            version: 1,
            id: Uuid::new_v4(),
            request_id: context.request_id,
            correlation_id: context.correlation_id,
            source: tool.into(),
            created_at_ms: now_ms(),
            expires_at_ms: now_ms() + 300000,
            success: false,
            data: Value::Null,
            evidence_refs: Vec::new(),
            error: None,
        };
        let Some(entry) = entry else {
            observation.error = Some("backend_unavailable".into());
            event(ToolState::Failed, None);
            return observation;
        };
        if let Err(error) = validate(&entry.descriptor.input_schema, &input) {
            observation.error = Some(error);
            event(ToolState::Failed, None);
            return observation;
        }
        let class = {
            let mut classes = self.classes.lock().unwrap();
            classes
                .entry(entry.descriptor.resource_class.clone())
                .or_insert_with(|| {
                    Arc::new(Semaphore::new(
                        if ["cpu", "sandbox"].contains(&entry.descriptor.resource_class.as_str()) {
                            1
                        } else {
                            4
                        },
                    ))
                })
                .clone()
        };
        let active_token = Arc::new(Mutex::new(token));
        let job = async {
            event(ToolState::PolicyPending, None);
            let capability = entry.backend.capability(tool, &input);
            let mut grant = token;
            let mut pending = None;
            if grant.is_none()
                && capability.risk_level() != sail_guardian::RiskLevel::Low
                && !self.guardian.policy().is_explicitly_granted_for(
                    context.actor,
                    &capability,
                    Some(context.correlation_id),
                )
            {
                let session = self.guardian.approval().create_session_for(
                    Some(context.correlation_id),
                    context.actor,
                    capability.clone(),
                    "Scheduled tool requires exact owner approval",
                );
                let session_id = session.session_id;
                pending = Some(PendingApproval {
                    broker: self.guardian.approval().clone(),
                    session: session_id,
                });
                event(ToolState::PolicyPending, Some(session_id));
                loop {
                    match self.guardian.approval().resolved_token(session_id) {
                        Ok(Some(token)) => {
                            grant = Some(token);
                            break;
                        }
                        Ok(None) => tokio::time::sleep(Duration::from_millis(25)).await,
                        Err(error) => return Err(error.to_string()),
                    }
                }
            }
            // Keep the approval alive until the operation finishes. Cancellation
            // or dropping the operation revokes it; execution never gets a new ID.
            let _pending = pending;
            *active_token.lock().unwrap() = grant;
            let _background = if foreground {
                None
            } else {
                Some(self.background.acquire().await.map_err(|e| e.to_string())?)
            };
            let _slots = self.slots.acquire().await.map_err(|e| e.to_string())?;
            let _class = class.acquire().await.map_err(|e| e.to_string())?;
            let _tool = entry.slots.acquire().await.map_err(|e| e.to_string())?;
            let _write = if entry.descriptor.security_effect == ToolEffect::ReadOnly {
                None
            } else {
                Some(self.writes.acquire().await.map_err(|e| e.to_string())?)
            };
            // Consume at the point of dispatch, after queueing. A grant that
            // expired or was revoked while waiting cannot start an effect.
            if let Err(error) = self.guardian.authorize_and_execute(
                context.actor,
                capability,
                grant,
                Some(context.correlation_id),
                || (),
            ) {
                if let sail_guardian::ExecutionDenied::ApprovalRequired { session_id, .. } = &error
                {
                    self.guardian.approval().revoke_session(*session_id);
                }
                return Err(error.to_string());
            }
            event(ToolState::Executing, None);
            let mut attempt = 0;
            loop {
                match entry.backend.execute(context.clone(), input.clone()).await {
                    Ok(output) => {
                        validate(&entry.descriptor.output_schema, &output)?;
                        return Ok(output);
                    }
                    Err(error) => {
                        if !error.retryable
                            || entry.descriptor.security_effect != ToolEffect::ReadOnly
                            || attempt >= 2
                        {
                            return Err(error.message);
                        }
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(50 * (1 << attempt))).await;
                    }
                }
            }
        };
        let revoked = async {
            loop {
                let token = *active_token.lock().unwrap();
                if token.is_some_and(|token| self.guardian.approval().is_revoked(token)) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        let outcome = tokio::select! {biased;_=context.cancel.cancelled()=>Err("cancelled".to_string()),_=revoked=>Err("revoked".to_string()),result=tokio::time::timeout(Duration::from_millis(entry.descriptor.timeout_ms),job)=>result.unwrap_or_else(|_|Err("deadline_exceeded".into()))};
        match outcome {
            Ok(output) => {
                let output = sail_common::diagnostics::redact_json(&output);
                let bytes = bounded_json(&output, 1024 * 1024);
                if bytes.is_err() {
                    observation.error = Some("tool_output_limit".into());
                } else {
                    let bytes = bytes.unwrap();
                    observation.success = true;
                    if bytes.len() > 4096 {
                        let length = bytes.len();
                        let id = self.content.lock().unwrap().put(
                            context.correlation_id,
                            context.actor,
                            bytes,
                            now_ms(),
                        );
                        observation.data = json!({"payload_ref":id,"bytes":length});
                        observation.evidence_refs.push(id);
                    } else {
                        observation.data = output;
                    }
                }
            }
            Err(error) => {
                observation.error = Some(
                    sail_common::diagnostics::redact(&error)
                        .chars()
                        .take(2048)
                        .collect(),
                );
            }
        }
        if !observation.success {
            context.cancel.cancel();
        }
        event(
            if observation.success {
                ToolState::Completed
            } else if observation.error.as_deref() == Some("cancelled") {
                ToolState::Cancelled
            } else {
                ToolState::Failed
            },
            None,
        );
        observation
    }
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
struct PendingApproval {
    broker: Arc<sail_guardian::ApprovalBroker>,
    session: Uuid,
}
impl Drop for PendingApproval {
    fn drop(&mut self) {
        self.broker.revoke_session(self.session);
    }
}
pub struct LocalTime;
#[async_trait]
impl Tool for LocalTime {
    async fn execute(&self, _context: ToolContext, _input: Value) -> Result<Value, ToolFailure> {
        Ok(json!({"utc":chrono::Utc::now().to_rfc3339(),"unix_ms":now_ms()}))
    }
}
pub fn register_builtin_time(registry: &ToolRegistry) -> Result<(), String> {
    registry.register(ToolDescriptor{id:"local:time".into(),version:1,description:"Read the current UTC date and time from this device".into(),input_schema:json!({"type":"object","properties":{},"additionalProperties":false}),output_schema:json!({"type":"object","properties":{"utc":{"type":"string","maxLength":64},"unix_ms":{"type":"integer","minimum":0}},"required":["utc","unix_ms"],"additionalProperties":false}),security_effect:ToolEffect::ReadOnly,required_capabilities:vec!["local:time".into()],timeout_ms:1000,resource_class:"telemetry".into(),concurrency_limit:4},Arc::new(LocalTime))
}

fn bounded_json(value: &Value, limit: usize) -> Result<Vec<u8>, serde_json::Error> {
    struct Writer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len() + bytes.len() > self.limit {
                return Err(std::io::Error::other("JSON budget exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}
