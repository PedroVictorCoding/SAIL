//! Projects the event log into chat messages for the local model (#239).
//!
//! The system prompt is built fresh for each turn and never stored. Tool calls left
//! without a result (abort, crash) get a synthetic "interrupted" result, results without a
//! matching call are dropped, and a question's result is the learner's recorded answer.
use super::events::{Envelope, LearningEvent};
use super::question::{Answer, Grade, QuestionSpec};
use sail_protocol::{FunctionCall, Message, Role, ToolCall};
use serde_json::json;

pub const INTERRUPTED: &str = "Interrupted before this call finished. Do not assume it ran.";
pub const AWAITING: &str = "Awaiting the learner's answer.";

fn tool_message(call_id: &str, content: String) -> Message {
    Message {
        role: Role::Tool,
        content: sail_protocol::Content::Text(content),
        tool_calls: Vec::new(),
        tool_call_id: Some(call_id.to_string()),
        name: None,
    }
}

/// The text a model sees as a question's result.
pub fn answer_result(spec: &QuestionSpec, answer: &Answer, grade: Grade) -> String {
    let chosen: Vec<&str> = answer
        .choices
        .iter()
        .filter_map(|id| spec.options.iter().find(|o| &o.id == id))
        .map(|o| o.text.as_str())
        .collect();
    json!({
        "learner_answer": {
            "chosen": chosen,
            "text": answer.text,
            "confidence_1_to_5": answer.confidence,
            "thinking": answer.thinking,
        },
        "grade": grade,
        "note": if grade == Grade::NeedsJudgment {
            "Judge this free answer yourself: say what is right, and name the specific missing or wrong idea."
        } else {
            "Graded by option id. Address the reasoning, not just the result."
        },
    })
    .to_string()
}

pub fn to_llm_messages(system_prompt: &str, events: &[Envelope]) -> Vec<Message> {
    let mut out = vec![Message::text(Role::System, system_prompt)];
    // Calls of the latest assistant message that have no result yet, in order.
    let mut pending: Vec<String> = Vec::new();
    let mut questions: Vec<(String, &QuestionSpec)> = Vec::new();

    fn flush(out: &mut Vec<Message>, pending: &mut Vec<String>) {
        for id in pending.drain(..) {
            out.push(tool_message(&id, INTERRUPTED.to_string()));
        }
    }

    for envelope in events {
        match &envelope.event {
            LearningEvent::SessionCreated { goal } => {
                flush(&mut out, &mut pending);
                out.push(Message::text(
                    Role::User,
                    format!("My learning goal: {goal}"),
                ));
            }
            LearningEvent::LearnerMessage { text } => {
                flush(&mut out, &mut pending);
                out.push(Message::text(Role::User, text.clone()));
            }
            LearningEvent::AssistantMessage {
                text, tool_calls, ..
            } => {
                flush(&mut out, &mut pending);
                let mut message = Message::text(Role::Assistant, text.clone());
                message.tool_calls = tool_calls
                    .iter()
                    .map(|c| ToolCall {
                        id: c.id.clone(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: c.name.clone(),
                            arguments: c.arguments.clone(),
                        },
                    })
                    .collect();
                pending = tool_calls.iter().map(|c| c.id.clone()).collect();
                out.push(message);
            }
            LearningEvent::ToolResult {
                call_id, content, ..
            } => {
                if let Some(i) = pending.iter().position(|id| id == call_id) {
                    pending.remove(i);
                    out.push(tool_message(call_id, content.clone()));
                }
            }
            LearningEvent::QuestionAsked { call_id, spec } => {
                questions.push((call_id.clone(), spec));
            }
            LearningEvent::Answered {
                call_id,
                answer,
                grade,
            } => {
                if let Some(i) = pending.iter().position(|id| id == call_id)
                    && let Some((_, spec)) = questions.iter().find(|(id, _)| id == call_id)
                {
                    pending.remove(i);
                    out.push(tool_message(call_id, answer_result(spec, answer, *grade)));
                }
            }
            LearningEvent::ProbeDone { .. }
            | LearningEvent::PlanUpdated { .. }
            | LearningEvent::ConceptPresented { .. }
            | LearningEvent::TurnSettled { .. }
            | LearningEvent::Paused
            | LearningEvent::Resumed => {}
        }
    }
    // At the end, an unanswered question keeps its placeholder; anything else was cut off.
    for id in pending.drain(..) {
        let asked = questions.iter().any(|(q, _)| *q == id);
        out.push(tool_message(
            &id,
            if asked { AWAITING } else { INTERRUPTED }.to_string(),
        ));
    }
    out
}

/// Mind's request bounds (see `sail-mind` validation): at most 128 messages and 128 KiB of
/// input including tool definitions. We stay below both with room for the tools.
pub const MAX_MESSAGES: usize = 120;
pub const MAX_BYTES: usize = 96 * 1024;

fn size(message: &Message) -> usize {
    let text = match &message.content {
        sail_protocol::Content::Text(t) => t.len(),
        sail_protocol::Content::Parts(_) => 0,
    };
    text + message
        .tool_calls
        .iter()
        .map(|c| c.function.arguments.len() + c.function.name.len())
        .sum::<usize>()
}

/// Keeps the system prompt, the goal message and the most recent history that fits. The
/// cut is always placed on a user message, so no tool call is separated from its result.
/// Full history stays in the store; this only bounds one request. (#252 adds a digest.)
pub fn window(messages: Vec<Message>) -> Vec<Message> {
    let total: usize = messages.iter().map(size).sum();
    if messages.len() <= MAX_MESSAGES && total <= MAX_BYTES {
        return messages;
    }
    let head = 2.min(messages.len());
    let note = Message::text(
        Role::User,
        "(Earlier parts of this lesson are omitted to fit the model's context.)",
    );
    let head_size: usize = messages[..head].iter().map(size).sum::<usize>() + size(&note);
    // Walk back from the end, remembering the earliest user-message boundary that fits.
    let (mut count, mut bytes) = (head + 1, head_size);
    let mut best = None;
    for i in (head..messages.len()).rev() {
        count += 1;
        bytes += size(&messages[i]);
        if count > MAX_MESSAGES || bytes > MAX_BYTES {
            break;
        }
        if messages[i].role == Role::User {
            best = Some(i);
        }
    }
    let cut = best.unwrap_or(messages.len());
    let mut out: Vec<Message> = messages[..head].to_vec();
    out.push(note);
    out.extend_from_slice(&messages[cut..]);
    out
}
