//! The small, privacy-safe subset of Hermes plugin hooks SAIL runs.
//!
//! A plugin registers hooks with `ctx.register_hook(name, callback)`. SAIL calls only
//! the four hooks below, never with conversation history, memory or tool results beyond
//! a short result preview. Claude runs the process and calls [`interpret`] on the reply.
use crate::market::{Bundle, Kind, Severity, guard};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Hooks SAIL runs. Every other hook is listed as "not run in SAIL".
pub const SUPPORTED_HOOKS: [&str; 4] = [
    "pre_llm_call",
    "post_llm_call",
    "pre_tool_call",
    "post_tool_call",
];

/// The most context characters a `pre_llm_call` hook may add.
pub const MAX_HOOK_CONTEXT_CHARS: usize = 2000;

/// The request body for one `op = "hook"` call to `host.py`.
pub fn hook_request(
    plugin_id: &str,
    plugin_dir: &str,
    settings: &Value,
    payload: &HookPayload,
) -> Value {
    serde_json::json!({
        "op": "hook",
        "plugin_id": plugin_id,
        "plugin_dir": plugin_dir,
        "settings": settings,
        "payload": payload,
    })
}

/// What one supported hook observes. Only these fields ever leave SAIL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "hook", rename_all = "snake_case")]
pub enum HookPayload {
    /// Before the model call.
    PreLlmCall {
        /// The current session.
        session_id: String,
        /// The owner's message for this turn.
        user_message: String,
        /// True on the first turn of a session.
        is_first_turn: bool,
        /// `web`, `cli` or another surface name.
        platform: String,
    },
    /// After the model call.
    PostLlmCall {
        /// The current session.
        session_id: String,
        /// The owner's message for this turn.
        user_message: String,
        /// The model's reply.
        assistant_response: String,
        /// `web`, `cli` or another surface name.
        platform: String,
    },
    /// Before a tool runs.
    PreToolCall {
        /// The current session.
        session_id: String,
        /// The tool's name.
        tool_name: String,
        /// The tool's arguments.
        args: Value,
    },
    /// After a tool runs.
    PostToolCall {
        /// The current session.
        session_id: String,
        /// The tool's name.
        tool_name: String,
        /// The tool's arguments.
        args: Value,
        /// Whether the tool succeeded.
        ok: bool,
        /// At most 1000 characters of the result, supplied by the caller.
        result_preview: String,
    },
}

/// What SAIL does with a hook's reply.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEffect {
    /// Nothing to do.
    None,
    /// Add this text to the turn's context.
    Context(String),
    /// Block the tool call with this message.
    Block(String),
    /// The reply was unusable or unsafe; log the reason and continue the turn.
    Rejected(String),
}

/// Interpret `host.py`'s `{"ok": true, "results": […]}` reply for one hook.
pub fn interpret(hook: &str, response: &Value) -> HookEffect {
    let Some(object) = response.as_object() else {
        return HookEffect::Rejected("the hook returned a non-object reply".into());
    };
    if object.get("ok").and_then(Value::as_bool) != Some(true) {
        let reason = object
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the hook failed");
        return HookEffect::Rejected(reason.to_string());
    }
    if !SUPPORTED_HOOKS.contains(&hook) {
        return HookEffect::Rejected(format!("unknown hook {hook}"));
    }
    let Some(results) = object.get("results").and_then(Value::as_array) else {
        return HookEffect::Rejected("the hook reply has no results".into());
    };
    match hook {
        "pre_llm_call" => interpret_context(results),
        "pre_tool_call" => interpret_block(results),
        "post_llm_call" | "post_tool_call" => HookEffect::None,
        _ => HookEffect::Rejected(format!("unknown hook {hook}")),
    }
}

fn interpret_context(results: &[Value]) -> HookEffect {
    let mut parts = Vec::new();
    for result in results {
        let text = match result {
            Value::String(text) => text.clone(),
            Value::Object(object) => match object.get("context").and_then(Value::as_str) {
                Some(text) => text.to_string(),
                None => {
                    return HookEffect::Rejected(
                        "a pre_llm_call result has no context string".into(),
                    );
                }
            },
            _ => {
                return HookEffect::Rejected(
                    "a pre_llm_call result is neither text nor a context object".into(),
                );
            }
        };
        parts.push(text);
    }
    if parts.is_empty() {
        return HookEffect::None;
    }
    let context = parts.join("\n\n");
    let bytes = context.as_bytes().to_vec();
    let bundle = Bundle {
        files: BTreeMap::from([("hook.md".to_string(), bytes)]),
        revision: String::new(),
    };
    let report = guard::scan(&bundle, Kind::Skill);
    if let Some(finding) = report
        .findings
        .iter()
        .find(|finding| finding.severity >= Severity::High && finding.category != "structure")
    {
        return HookEffect::Rejected(format!(
            "hook context failed the scanner ({})",
            finding.rule
        ));
    }
    HookEffect::Context(take_chars(&context, MAX_HOOK_CONTEXT_CHARS))
}

fn interpret_block(results: &[Value]) -> HookEffect {
    for result in results {
        let Some(object) = result.as_object() else {
            continue;
        };
        if object.get("action").and_then(Value::as_str) != Some("block") {
            continue;
        }
        let message = object
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the plugin blocked this tool call");
        return HookEffect::Block(take_chars(message, 300));
    }
    HookEffect::None
}

fn take_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
