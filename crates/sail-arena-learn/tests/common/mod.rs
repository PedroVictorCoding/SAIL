//! Shared scripted model for tests (included with `mod common_model;`).
#![allow(dead_code)]
use sail_arena_learn::Model;
use sail_protocol::*;
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub enum Step {
    Reply(Message),
    Fail(String),
    /// Waits until cancelled.
    Hang,
}

#[derive(Default)]
pub struct Scripted {
    steps: Mutex<VecDeque<Step>>,
    pub requests: Mutex<Vec<InferenceRequest>>,
}

impl Scripted {
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::default(),
        }
    }
    pub fn push(&self, step: Step) {
        self.steps.lock().unwrap().push_back(step);
    }
    pub fn remaining(&self) -> usize {
        self.steps.lock().unwrap().len()
    }
}

pub fn call(name: &str, args: serde_json::Value) -> Step {
    calls("", &[(name, args)])
}

pub fn calls(text: &str, list: &[(&str, serde_json::Value)]) -> Step {
    let mut m = Message::text(Role::Assistant, text);
    // Deliberately reuse ids, as small local models do; the engine must replace them.
    m.tool_calls = list
        .iter()
        .map(|(name, args)| ToolCall {
            id: "call_0".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        })
        .collect();
    Step::Reply(m)
}

pub fn say(text: &str) -> Step {
    Step::Reply(Message::text(Role::Assistant, text))
}

#[async_trait::async_trait]
impl Model for Scripted {
    async fn complete(
        &self,
        request: InferenceRequest,
        cancel: CancellationToken,
    ) -> std::result::Result<InferenceResponse, String> {
        // The engine must always send a request Mind would accept.
        sail_mind::validate_request(&request, &ResourceLimits::default()).map_err(|e| e.message)?;
        self.requests.lock().unwrap().push(request.clone());
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .ok_or("script exhausted")?;
        match step {
            Step::Reply(message) => Ok(InferenceResponse {
                version: VERSION,
                request_id: request.request_id,
                model: "scripted".into(),
                message,
                finish_reason: "stop".into(),
                usage: Usage {
                    input_tokens: Some(100),
                    output_tokens: Some(10),
                },
                timings: Timings {
                    queue_ms: 0,
                    first_token_ms: None,
                    inference_ms: 1,
                },
            }),
            Step::Fail(message) => Err(message),
            Step::Hang => {
                cancel.cancelled().await;
                Err("cancelled".into())
            }
        }
    }
}
