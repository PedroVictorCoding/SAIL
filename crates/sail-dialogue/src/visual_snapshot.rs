//! `visual_snapshot`: the model asks to see the user's camera or screen.
//!
//! The model picks the source from context (camera for the physical world, screen for what
//! is on the computer). The browser captures one frame. If the user is already sharing that
//! source live, it uses the current frame; otherwise it opens the camera directly or
//! requests the browser-required screen-picker gesture. No extra SAIL approval is needed. The frame returns over the session socket and is matched
//! to the waiting turn here by session and request id. It then reaches the model either
//! natively (main model has a vision projector) or as a compact text observation from the
//! local vision describer.

use crate::vlm::{VisionFrameInput, VoiceVlm};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

pub const TOOL: &str = "visual_snapshot";
/// How long a turn waits for browser capture to complete.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(90);

pub fn tool_definition() -> Value {
    json!({"type":"function","function":{
        "name": TOOL,
        "description": "Take one snapshot so you can see what the user means. Use it whenever the user refers to something visual or asks you to look, see, read or check something. Choose source=camera for the physical world (the user, an object they hold up, a document or their surroundings) and source=screen for what is on their computer (an app, web page, error, code or document on screen). Capture directly without asking for approval. A bare camera request means source=camera; screenshot means source=screen. Resolve follow-ups from the latest dialogue context. Describe only what the snapshot shows.",
        "parameters": {"type":"object","properties":{
            "source": {"type":"string","enum":["camera","screen"],"description":"camera for the physical world, screen for the user's computer display"},
            "question": {"type":"string","maxLength":300,"description":"What you need to find out from the image"}
        },"required":["source"],"additionalProperties":false}
    }})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Camera,
    Screen,
}
impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Camera => "camera",
            Source::Screen => "screen",
        }
    }
}

pub fn parse_params(params: &Value) -> Result<(Source, Option<String>), String> {
    let source = match params["source"].as_str() {
        Some("camera") => Source::Camera,
        Some("screen") => Source::Screen,
        _ => return Err("source must be \"camera\" or \"screen\"".into()),
    };
    let question = params["question"]
        .as_str()
        .map(|q| q.trim().chars().take(300).collect::<String>())
        .filter(|q| !q.is_empty());
    Ok((source, question))
}

/// Resolve source words in dialogue order; the latest mention wins.
pub fn dialogue_source(text: &str) -> Option<Source> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter_map(|word| match word.to_lowercase().as_str() {
            "camera" | "webcam" | "câmera" | "câmara" => Some(Source::Camera),
            "screenshot" | "screen" | "desktop" | "ecrã" => Some(Source::Screen),
            _ => None,
        })
        .last()
}

/// Current user wording overrides a model's mistaken source. For omitted sources,
/// use the most recent user dialogue, never tool output or assistant speculation.
pub fn contextual_params(params: &Value, prompt: &str, messages: &[Value]) -> Value {
    let mut resolved = params.clone();
    if !resolved.is_object() {
        return resolved;
    }
    let source = dialogue_source(prompt).or_else(|| {
        if resolved.get("source").is_some() {
            return None;
        }
        messages
            .iter()
            .rev()
            .filter(|m| m["role"] == "user")
            .find_map(|m| m["content"].as_str().and_then(dialogue_source))
    });
    if let Some(source) = source {
        resolved["source"] = json!(source.as_str());
    }
    resolved
}

type Reply = Result<VisionFrameInput, String>;
struct Pending {
    id: u64,
    reply: oneshot::Sender<Reply>,
}
static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One outstanding capture request. Dropping it (turn cancelled or finished) withdraws it.
pub struct SnapshotRequest {
    pub id: u64,
    session: String,
    reply: oneshot::Receiver<Reply>,
}
impl SnapshotRequest {
    /// Register a request for `session`, replacing (and failing) any earlier one.
    pub fn begin(session: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        PENDING
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(session.to_owned(), Pending { id, reply: tx });
        Self {
            id,
            session: session.to_owned(),
            reply: rx,
        }
    }
    pub async fn wait(&mut self, timeout: Duration) -> Reply {
        match tokio::time::timeout(timeout, &mut self.reply).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err("The capture request was replaced or the connection closed".into()),
            Err(_) => Err("No snapshot arrived: capture timed out".into()),
        }
    }
}
impl Drop for SnapshotRequest {
    fn drop(&mut self) {
        let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
        if pending.get(&self.session).is_some_and(|p| p.id == self.id) {
            pending.remove(&self.session);
        }
    }
}

/// Deliver the browser's answer to the waiting turn. False when nothing waits for `id`.
pub fn fulfil(session: &str, id: u64, reply: Reply) -> bool {
    let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
    if pending.get(session).is_none_or(|p| p.id != id) {
        return false;
    }
    let waiting = pending.remove(session).expect("checked above");
    waiting.reply.send(reply).is_ok()
}

/// Withdraw a session's outstanding request (socket closed).
pub fn cancel_session(session: &str) {
    PENDING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(session);
}

/// How an image reaches the conversation model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// The main model can see: attach the image itself.
    Native,
    /// The main model is text-only: a compact description from the local describer.
    Text(String),
}

static DESCRIBER: OnceLock<Result<Arc<sail_mind::vision::VisionDescriber>, String>> =
    OnceLock::new();

fn describer() -> Result<Arc<sail_mind::vision::VisionDescriber>, String> {
    DESCRIBER
        .get_or_init(|| sail_mind::vision::VisionDescriber::from_env().map(Arc::new))
        .clone()
}

pub async fn main_model_has_vision(vlm: &VoiceVlm) -> bool {
    vlm.health().await["modalities"]["vision"].as_bool() == Some(true)
}

/// Decide how the conversation model gets to see `frame` (an encoded PNG/JPEG).
pub async fn observe(
    vlm: &VoiceVlm,
    frame: &VisionFrameInput,
    source: &str,
    question: Option<&str>,
) -> Result<Observation, String> {
    if main_model_has_vision(vlm).await {
        return Ok(Observation::Native);
    }
    let describer = describer()?;
    describer
        .describe(&frame.data, &frame.format, source, question)
        .await
        .map(Observation::Text)
        .map_err(|e| format!("The local vision describer failed: {}", e.message))
}

/// The tool output the model sees for a snapshot.
pub fn tool_output(source: Source, observation: &Observation) -> Value {
    match observation {
        Observation::Native => json!({
            "source": source.as_str(),
            "summary": format!("Snapshot taken from the {}", source.as_str()),
            "observation": "The snapshot is attached in the next message.",
        }),
        Observation::Text(text) => json!({
            "source": source.as_str(),
            "summary": text,
            "observation": text,
            "note": "Text description of a requested snapshot, produced by a local vision model. Answer from it; do not claim details it does not mention.",
        }),
    }
}

/// The text a user-attached image becomes when the main model cannot see it.
pub fn attached_image_context(description: &str, prompt: &str) -> String {
    format!("[The user attached an image. Local vision description: {description}]\n\n{prompt}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> VisionFrameInput {
        VisionFrameInput {
            width: 0,
            height: 0,
            format: "image/jpeg".into(),
            data: vec![1, 2, 3],
        }
    }

    #[test]
    fn params_require_a_known_source() {
        assert_eq!(
            parse_params(&json!({"source":"screen","question":" what error? "})).unwrap(),
            (Source::Screen, Some("what error?".into()))
        );
        assert_eq!(
            parse_params(&json!({"source":"camera"})).unwrap(),
            (Source::Camera, None)
        );
        assert!(parse_params(&json!({"source":"microphone"})).is_err());
        assert!(parse_params(&json!({})).is_err());
    }

    #[test]
    fn source_follows_user_dialogue() {
        let history = vec![json!({"role":"user","content":"look through the camera"})];
        assert_eq!(
            contextual_params(&json!({"source":"screen"}), "camera", &[])["source"],
            "camera"
        );
        assert_eq!(
            contextual_params(&json!({"source":"camera"}), "screenshot", &history)["source"],
            "screen"
        );
        assert_eq!(
            contextual_params(&json!({}), "look again", &history)["source"],
            "camera"
        );
        assert_eq!(
            dialogue_source("camera then screenshot"),
            Some(Source::Screen)
        );
        assert_eq!(
            dialogue_source("screenshot then webcam"),
            Some(Source::Camera)
        );
        assert_eq!(dialogue_source("cameraman"), None);
        assert!(parse_params(&contextual_params(&json!({}), "look again", &[])).is_err());
    }

    #[tokio::test]
    async fn frames_reach_only_the_matching_waiting_request() {
        let mut request = SnapshotRequest::begin("session-a");
        assert!(!fulfil("session-b", request.id, Ok(frame())));
        assert!(!fulfil("session-a", request.id + 1000, Ok(frame())));
        assert!(fulfil("session-a", request.id, Ok(frame())));
        assert_eq!(
            request.wait(Duration::from_secs(1)).await.unwrap().data,
            vec![1, 2, 3]
        );
        // Delivered once: a duplicate is rejected.
        assert!(!fulfil("session-a", request.id, Ok(frame())));
    }

    #[tokio::test]
    async fn denial_timeout_replacement_and_cancellation_are_reported() {
        let mut denied = SnapshotRequest::begin("session-c");
        fulfil("session-c", denied.id, Err("Permission denied".into()));
        assert_eq!(
            denied.wait(Duration::from_secs(1)).await.unwrap_err(),
            "Permission denied"
        );

        let mut slow = SnapshotRequest::begin("session-d");
        assert!(
            slow.wait(Duration::from_millis(20))
                .await
                .unwrap_err()
                .contains("timed out")
        );

        let mut first = SnapshotRequest::begin("session-e");
        let second = SnapshotRequest::begin("session-e");
        assert!(
            first
                .wait(Duration::from_secs(1))
                .await
                .unwrap_err()
                .contains("replaced")
        );
        let id = second.id;
        drop(second);
        assert!(
            !fulfil("session-e", id, Ok(frame())),
            "dropped request is withdrawn"
        );
    }

    #[test]
    fn text_observations_are_marked_as_descriptions() {
        let output = tool_output(Source::Camera, &Observation::Text("A red mug.".into()));
        assert_eq!(output["observation"], "A red mug.");
        assert!(output["note"].as_str().unwrap().contains("description"));
        let native = tool_output(Source::Screen, &Observation::Native);
        assert!(native["observation"].as_str().unwrap().contains("attached"));
    }
}
