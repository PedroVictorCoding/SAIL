//! Lightweight local vision describer.
//!
//! The resident conversation model may have no vision projector. In that case an image
//! (camera picture, screenshot, upload) is turned into a short text observation by a small
//! multimodal model, LiquidAI LFM3-Omni-2B, which runs as its own managed llama-server.
//! The main model then receives bounded text instead of pixels (vision §Vision: "the text
//! model should receive bounded, permissioned observations").
//!
//! Weights are never downloaded implicitly; `scripts/fetch-vision-model.sh` fetches them.
//! The server is started on first use and follows `SAIL_LLAMA_PERSIST` like the main model.

use crate::{config::LocalModelConfig, service::MindService};
use sail_protocol::{
    Content, ContentPart, ErrorCode, Generation, ImageUrl, InferenceError, InferenceRequest,
    Message, ResourceLimits, Result, Role, resources::WorkPriority,
};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_MODEL_FILE: &str = "LFM3-Omni-2B-Q4_K_M.gguf";
pub const DEFAULT_PROJECTOR_FILE: &str = "mmproj-LFM3-Omni-2B-Q8_0.gguf";
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8082/v1/chat/completions";
/// Upper bound on the observation handed to the conversation model.
pub const MAX_OBSERVATION_CHARS: usize = 1200;

const SYSTEM_PROMPT: &str =
    "Answer questions about the provided image. Base every answer only on what can be seen.";

/// Where the describer weights are looked for, in order.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(dir) = std::env::var("SAIL_VISION_MODEL_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    let runtime = std::env::var("SAIL_RUNTIME").unwrap_or_else(|_| "/srv/sail".into());
    dirs.push(Path::new(&runtime).join("models/vision"));
    dirs.push(PathBuf::from("models/vision"));
    dirs
}

fn locate(env: &str, file: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(env) {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    search_dirs()
        .into_iter()
        .map(|dir| dir.join(file))
        .find(|p| p.is_file())
}

/// Describer model configuration, or why it is unavailable.
pub fn describer_config_from_env() -> std::result::Result<LocalModelConfig, String> {
    let missing = || {
        format!(
            "The local vision describer is not installed ({DEFAULT_MODEL_FILE} and {DEFAULT_PROJECTOR_FILE}). Run scripts/fetch-vision-model.sh, then restart SAIL."
        )
    };
    let model_path = locate("SAIL_VISION_MODEL", DEFAULT_MODEL_FILE).ok_or_else(missing)?;
    let projector = locate("SAIL_VISION_MMPROJ", DEFAULT_PROJECTOR_FILE).ok_or_else(missing)?;
    let endpoint = std::env::var("SAIL_VISION_URL").unwrap_or_else(|_| DEFAULT_ENDPOINT.into());
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let base = LocalModelConfig::default();
    Ok(LocalModelConfig {
        name: "LFM3-Omni-2B".into(),
        aliases: vec!["vision-describer".into()],
        model_path,
        projector: Some(projector),
        managed: std::env::var("SAIL_VISION_URL").is_err(),
        endpoint,
        // A 2B model describes one image in seconds on CPU; leave any GPU to the main model.
        // The main model is idle while a turn waits for a description, so image encoding
        // (prompt processing) may use every core.
        compute: sail_protocol::ComputeBackend::Cpu,
        threads: (cpus / 2).clamp(1, 6),
        threads_batch: Some(cpus.clamp(1, 12)),
        context_tokens: 4096,
        cache_type_k: "f16".into(),
        cache_type_v: "f16".into(),
        batch_size: 2048,
        // An image is decoded in one micro-batch; keep it above --image-max-tokens (1024).
        ubatch_size: 2048,
        cache_reuse: 0,
        ..base
    })
}

/// Build the request that asks for a compact, question-guided observation.
pub fn describe_request(
    image: &[u8],
    mime: &str,
    source: &str,
    question: Option<&str>,
) -> InferenceRequest {
    use base64::Engine;
    let what = match source {
        "screen" => "a screenshot of the user's screen",
        "camera" => "a picture from the user's camera",
        _ => "an image the user shared",
    };
    let focus = question
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!(" The assistant needs it to answer: \"{q}\"."))
        .unwrap_or_default();
    let instruction = format!(
        "This is {what}.{focus} Describe it for an assistant that cannot see it, in at most 70 words of plain text: first one sentence on what it shows overall, then the details that matter for the question, then any important readable text quoted exactly (titles, error messages, labels). No preamble. Do not guess what is not visible."
    );
    let url = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(image)
    );
    InferenceRequest {
        messages: vec![
            Message::text(Role::System, SYSTEM_PROMPT),
            Message {
                role: Role::User,
                content: Content::Parts(vec![
                    ContentPart::ImageUrl {
                        image_url: ImageUrl { url },
                    },
                    ContentPart::Text { text: instruction },
                ]),
                ..Message::text(Role::User, "")
            },
        ],
        generation: Generation {
            max_tokens: 200,
            // The model card recommends greedy decoding for text output.
            temperature: 0.0,
            reasoning_budget: None,
        },
        ..InferenceRequest::text("")
    }
}

/// Collapse whitespace and bound the observation at a word boundary.
pub fn compact_observation(text: &str) -> String {
    let text = text
        .replace("<think>", "")
        .replace("</think>", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() <= MAX_OBSERVATION_CHARS {
        return text;
    }
    let cut: String = text.chars().take(MAX_OBSERVATION_CHARS).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{cut}…")
}

pub struct VisionDescriber {
    service: MindService,
}

impl VisionDescriber {
    pub fn from_env() -> std::result::Result<Self, String> {
        let config = describer_config_from_env()?;
        Self::new(config).map_err(|e| e.message)
    }
    pub fn new(config: LocalModelConfig) -> Result<Self> {
        let limits = ResourceLimits {
            max_parallel: 1,
            ..ResourceLimits::default()
        };
        Ok(Self {
            service: MindService::new(config, limits)?,
        })
    }
    /// Describe one PNG/JPEG image as compact text, focused on `question` when given.
    pub async fn describe(
        &self,
        image: &[u8],
        mime: &str,
        source: &str,
        question: Option<&str>,
    ) -> Result<String> {
        let request = describe_request(image, mime, source, question);
        let response = self
            .service
            .infer_with_priority(request, CancellationToken::new(), None, WorkPriority::Voice)
            .await?;
        let text = compact_observation(response.message.content.text().unwrap_or_default());
        if text.is_empty() {
            return Err(InferenceError::new(
                ErrorCode::BackendProtocol,
                "The vision describer returned no description",
            ));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_carries_image_question_and_greedy_bounded_generation() {
        let request = describe_request(
            b"jpeg",
            "image/jpeg",
            "screen",
            Some("What error is shown?"),
        );
        assert_eq!(request.generation.temperature, 0.0);
        assert!(request.generation.max_tokens <= 200);
        let Content::Parts(parts) = &request.messages[1].content else {
            panic!("user message must be multimodal");
        };
        assert!(
            matches!(&parts[0], ContentPart::ImageUrl { image_url } if image_url.url.starts_with("data:image/jpeg;base64,"))
        );
        let ContentPart::Text { text } = &parts[1] else {
            panic!("instruction expected")
        };
        assert!(text.contains("screenshot"));
        assert!(text.contains("What error is shown?"));
    }
    #[test]
    fn observations_are_compacted_and_bounded() {
        assert_eq!(
            compact_observation("<think></think>  A desk.\n\nA mug  "),
            "A desk. A mug"
        );
        let long = "word ".repeat(1000);
        let compact = compact_observation(&long);
        assert!(compact.chars().count() <= MAX_OBSERVATION_CHARS + 1);
        assert!(compact.ends_with('…'));
    }
}
