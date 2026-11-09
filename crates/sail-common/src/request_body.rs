//! The chat-completions body SAIL sends to llama-server, built in one place so Mind, Forge
//! and the model eval (`sail_model_eval`) send exactly the same thing.
//!
//! Two things are decided here and nowhere else:
//!
//! - **Thinking.** Qwen3-family models think before answering unless told not to, and
//!   llama-server lets them think without limit by default. A request without a
//!   [`Generation::reasoning_budget`] is sent with thinking off. A request with one is sent
//!   with thinking on, `reasoning_budget_tokens` set, and room for the thinking added to
//!   `max_tokens`, so the answer keeps the full budget the caller asked for. Reasoning comes
//!   back separately (`reasoning_content`) and Mind never returns or streams it.
//! - **Sampling.** Each model family has the sampling its authors recommend, for answering
//!   directly and for thinking. The caller still chooses the temperature, except that Qwen
//!   thinking is never sampled greedily: its model card warns that greedy decoding makes the
//!   thinking repeat itself, so thinking requests use at least
//!   [`Sampling::min_reasoning_temperature`].

use sail_protocol::{Generation, InferenceRequest};
use serde_json::{Value, json};

/// Sampling settings for one model family and mode. `None` leaves llama-server's default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sampling {
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub min_p: Option<f32>,
    pub repeat_penalty: Option<f32>,
    /// Lowest temperature used while thinking; see the module docs.
    pub min_reasoning_temperature: Option<f32>,
}

impl Sampling {
    /// No overrides: llama-server's defaults, used for models SAIL has no profile for.
    pub const SERVER_DEFAULT: Sampling = Sampling {
        top_p: None,
        top_k: None,
        min_p: None,
        repeat_penalty: None,
        min_reasoning_temperature: None,
    };
}

/// What SAIL knows about how to sample one model family.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelFamily {
    pub name: &'static str,
    /// The chat template has a thinking mode that `enable_thinking` switches.
    pub thinks: bool,
    pub direct: Sampling,
    pub reasoned: Sampling,
}

/// Qwen3 / Qwen3.5 / Qwen3.6 model card: answering directly, temperature 0.7, top_p 0.8,
/// top_k 20, min_p 0; thinking, temperature 0.6, top_p 0.95, top_k 20, min_p 0, and no
/// greedy decoding.
const QWEN3: ModelFamily = ModelFamily {
    name: "qwen3",
    thinks: true,
    direct: Sampling {
        top_p: Some(0.8),
        top_k: Some(20),
        min_p: Some(0.0),
        repeat_penalty: None,
        min_reasoning_temperature: None,
    },
    reasoned: Sampling {
        top_p: Some(0.95),
        top_k: Some(20),
        min_p: Some(0.0),
        repeat_penalty: None,
        min_reasoning_temperature: Some(0.6),
    },
};

/// Liquid AI LFM2 / LFM2.5 / LFM3 model cards: min_p 0.15 and a light repetition penalty.
/// They have no thinking mode SAIL can switch, so a reasoning budget is ignored.
const LFM: ModelFamily = ModelFamily {
    name: "lfm",
    thinks: false,
    direct: Sampling {
        top_p: None,
        top_k: None,
        min_p: Some(0.15),
        repeat_penalty: Some(1.05),
        min_reasoning_temperature: None,
    },
    reasoned: Sampling {
        top_p: None,
        top_k: None,
        min_p: Some(0.15),
        repeat_penalty: Some(1.05),
        min_reasoning_temperature: None,
    },
};

/// A model SAIL has no profile for: server defaults, and thinking explicitly off.
const UNKNOWN: ModelFamily = ModelFamily {
    name: "unknown",
    thinks: false,
    direct: Sampling::SERVER_DEFAULT,
    reasoned: Sampling::SERVER_DEFAULT,
};

/// The family of a model, from its weights file name or served name (case-insensitive).
pub fn family_for(model: &str) -> ModelFamily {
    let name = model
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    if name.starts_with("qwen3") {
        QWEN3
    } else if name.starts_with("lfm") {
        LFM
    } else {
        UNKNOWN
    }
}

/// The thinking budget this model will really use for `generation`: `None` when the
/// request asks for none or the model can't think.
pub fn reasoning_budget(family: &ModelFamily, generation: &Generation) -> Option<u32> {
    generation
        .effective_reasoning_budget()
        .filter(|_| family.thinks)
}

/// Tokens the model may generate in total: the answer budget plus any thinking budget.
/// Mind uses this for the context-window check and the output-token check.
pub fn total_output_tokens(family: &ModelFamily, generation: &Generation) -> u32 {
    generation
        .max_tokens
        .saturating_add(reasoning_budget(family, generation).unwrap_or(0))
}

/// The thinking fields every body carries: on with a budget, or off.
fn reasoning_fields(body: &mut Value, family: &ModelFamily, generation: &Generation) {
    match reasoning_budget(family, generation) {
        Some(budget) => {
            body["chat_template_kwargs"] = json!({"enable_thinking": true});
            body["reasoning_budget_tokens"] = json!(budget);
            // Reasoning goes to `reasoning_content`, never into the answer text.
            body["reasoning_format"] = json!("deepseek");
        }
        None => {
            body["chat_template_kwargs"] = json!({"enable_thinking": false});
        }
    }
}

/// Body for `/apply-template`, used to count prompt tokens. It must render the same prompt
/// as the real request, so it carries the same thinking switch.
pub fn template_body(family: &ModelFamily, request: &InferenceRequest, messages: &Value) -> Value {
    let mut body = json!({
        "messages": messages,
        "add_generation_prompt": true,
        "tool_choice": request.tool_choice,
    });
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools);
    }
    reasoning_fields(&mut body, family, &request.generation);
    body
}

/// Body for `/v1/chat/completions`. `served` is the name sent as the request's `model`;
/// `family` (from [`family_for`] on the weights file) picks thinking and sampling.
pub fn chat_body(
    served: &str,
    family: &ModelFamily,
    request: &InferenceRequest,
    stream: bool,
) -> Value {
    let generation = &request.generation;
    let thinking = reasoning_budget(family, generation).is_some();
    let sampling = if thinking {
        family.reasoned
    } else {
        family.direct
    };
    let temperature = match sampling.min_reasoning_temperature {
        Some(floor) if thinking => generation.temperature.max(floor),
        _ => generation.temperature,
    };
    let mut body = json!({
        "model": served,
        "messages": request.messages,
        "stream": stream,
        "max_tokens": total_output_tokens(family, generation),
        "temperature": temperature,
        "tool_choice": request.tool_choice,
    });
    if stream {
        body["stream_options"] = json!({"include_usage": true});
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools);
    }
    for (key, value) in [
        ("top_p", sampling.top_p.map(|v| json!(v))),
        ("top_k", sampling.top_k.map(|v| json!(v))),
        ("min_p", sampling.min_p.map(|v| json!(v))),
        ("repeat_penalty", sampling.repeat_penalty.map(|v| json!(v))),
    ] {
        if let Some(value) = value {
            body[key] = value;
        }
    }
    reasoning_fields(&mut body, family, generation);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(generation: Generation) -> InferenceRequest {
        InferenceRequest {
            generation,
            ..InferenceRequest::text("hi")
        }
    }

    #[test]
    fn direct_qwen_request_turns_thinking_off_with_card_sampling() {
        let body = chat_body(
            "served",
            &family_for("Qwen3.6-35B-A3B-Q4_K_M.gguf"),
            &request(Generation::direct(384, 0.4)),
            true,
        );
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert!(body.get("reasoning_budget_tokens").is_none());
        assert_eq!(body["max_tokens"], 384);
        assert_eq!(body["top_k"], 20);
        assert!((body["top_p"].as_f64().unwrap() - 0.8).abs() < 1e-6);
        assert!((body["temperature"].as_f64().unwrap() - 0.4).abs() < 1e-6);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn reasoned_qwen_request_adds_budget_room_and_never_samples_greedily() {
        let body = chat_body(
            "served",
            &family_for("/srv/sail/models/Qwen3.6-35B-A3B-Q4_K_M.gguf"),
            &request(Generation::reasoned(512, 0.0, 1024)),
            false,
        );
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(body["reasoning_budget_tokens"], 1024);
        assert_eq!(body["reasoning_format"], "deepseek");
        assert_eq!(body["max_tokens"], 512 + 1024);
        assert!((body["temperature"].as_f64().unwrap() - 0.6).abs() < 1e-6);
        assert!((body["top_p"].as_f64().unwrap() - 0.95).abs() < 1e-6);
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn budget_is_clamped_and_zero_means_off() {
        let family = family_for("qwen3.6");
        let huge = Generation::reasoned(100, 0.6, 1_000_000);
        assert_eq!(
            reasoning_budget(&family, &huge),
            Some(sail_protocol::MAX_REASONING_BUDGET)
        );
        let raw = Generation {
            reasoning_budget: Some(0),
            ..Generation::direct(100, 0.6)
        };
        assert_eq!(reasoning_budget(&family, &raw), None);
        assert_eq!(total_output_tokens(&family, &raw), 100);
    }

    #[test]
    fn models_without_thinking_ignore_the_budget() {
        for model in ["LFM2.5-24B-A2B-Uncensored-AD-Q4_K_M.gguf", "mystery.gguf"] {
            let body = chat_body(
                model,
                &family_for(model),
                &request(Generation::reasoned(200, 0.3, 2048)),
                true,
            );
            assert_eq!(
                body["chat_template_kwargs"]["enable_thinking"], false,
                "{model}"
            );
            assert!(body.get("reasoning_budget_tokens").is_none(), "{model}");
            assert_eq!(body["max_tokens"], 200, "{model}");
        }
        let lfm_file = "LFM3-Omni-2B-Q4_K_M.gguf";
        let lfm = chat_body(
            lfm_file,
            &family_for(lfm_file),
            &request(Generation::default()),
            true,
        );
        assert!((lfm["min_p"].as_f64().unwrap() - 0.15).abs() < 1e-6);
        let unknown = chat_body(
            "mystery.gguf",
            &family_for("mystery.gguf"),
            &request(Generation::default()),
            true,
        );
        assert!(unknown.get("top_p").is_none() && unknown.get("min_p").is_none());
    }

    #[test]
    fn template_body_matches_the_thinking_switch_of_the_real_request() {
        let reasoned = request(Generation::reasoned(200, 0.6, 512));
        let messages = json!(reasoned.messages);
        let qwen = family_for("Qwen3.6");
        let body = template_body(&qwen, &reasoned, &messages);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(body["add_generation_prompt"], true);
        let direct = request(Generation::default());
        let body = template_body(&qwen, &direct, &messages);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
    }
}

/// Background work that may think. Live voice, Arena turns and anything else someone is
/// waiting on are deliberately absent: they always answer directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// Arena Learn's free-recall judge.
    Judge,
    /// Course Factory authoring (concepts, curriculum, lessons, review variants).
    CourseAuthoring,
    /// Moodle date and grading-rule extraction.
    MoodleExtraction,
    /// Background task plans (never interactive ones).
    Planning,
    /// Memory consolidation proposals.
    MemoryConsolidation,
}

impl Job {
    /// Every job, for docs and diagnostics.
    pub const ALL: [Job; 5] = [
        Job::Judge,
        Job::CourseAuthoring,
        Job::MoodleExtraction,
        Job::Planning,
        Job::MemoryConsolidation,
    ];

    /// The environment variable that overrides this job's budget: a number of tokens, or
    /// `0`/`off` for no thinking. Lets the owner trial a budget without a rebuild.
    pub fn env_var(self) -> &'static str {
        match self {
            Job::Judge => "SAIL_THINK_JUDGE",
            Job::CourseAuthoring => "SAIL_THINK_COURSE_AUTHORING",
            Job::MoodleExtraction => "SAIL_THINK_MOODLE",
            Job::Planning => "SAIL_THINK_PLANNING",
            Job::MemoryConsolidation => "SAIL_THINK_MEMORY",
        }
    }

    /// The built-in budget, set from the model eval (`docs/MODEL_EVAL.md`). A job stays at
    /// `None` until a measured run shows thinking helps it enough to pay for the time.
    pub fn default_budget(self) -> Option<u32> {
        match self {
            Job::Judge
            | Job::CourseAuthoring
            | Job::MoodleExtraction
            | Job::Planning
            | Job::MemoryConsolidation => None,
        }
    }

    /// The budget this job uses now: the environment override if it parses, else the
    /// built-in default. An unreadable override is ignored rather than guessed at.
    pub fn budget(self) -> Option<u32> {
        match std::env::var(self.env_var()) {
            Ok(value) => parse_budget(&value).unwrap_or_else(|| self.default_budget()),
            Err(_) => self.default_budget(),
        }
    }
}

/// `"off"`/`"0"` means `Some(None)` (explicitly off); a number is a budget; anything else is
/// `None` (not understood).
fn parse_budget(value: &str) -> Option<Option<u32>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("off") {
        return Some(None);
    }
    value
        .parse::<u32>()
        .ok()
        .map(|n| (n > 0).then_some(n.min(sail_protocol::MAX_REASONING_BUDGET)))
}

#[cfg(test)]
mod job_tests {
    use super::*;

    #[test]
    fn budget_overrides_parse_conservatively() {
        assert_eq!(parse_budget("512"), Some(Some(512)));
        assert_eq!(parse_budget(" off "), Some(None));
        assert_eq!(parse_budget("0"), Some(None));
        assert_eq!(
            parse_budget("999999"),
            Some(Some(sail_protocol::MAX_REASONING_BUDGET))
        );
        assert_eq!(parse_budget("lots"), None);
        assert_eq!(parse_budget("-5"), None);
    }

    #[test]
    fn every_job_has_its_own_override() {
        let mut vars: Vec<_> = Job::ALL.iter().map(|j| j.env_var()).collect();
        vars.sort();
        vars.dedup();
        assert_eq!(vars.len(), Job::ALL.len());
    }
}
