//! Projects the event log into chat messages for the local model (#239).
//!
//! The system prompt is built fresh for each turn and never stored. Tool calls left
//! without a result (abort, crash) get a synthetic "interrupted" result, results without a
//! matching call are dropped, and a question's result is the learner's recorded answer
//! together with its judgment, when free recall was judged (#243).
use super::events::{Envelope, LearningEvent};
use super::judgment::{Judgment, Verdict};
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

/// What the tutor is told to do with a graded or judged answer.
fn answer_note(spec: &QuestionSpec, grade: Grade, judgment: Option<&Judgment>) -> String {
    if grade != Grade::NeedsJudgment {
        return "Graded by option id. Address the reasoning, not just the result.".into();
    }
    let Some(judgment) = judgment else {
        return if spec.rubric.is_some() {
            "Not judged yet. Do not give a verdict; respond to the learner's reasoning.".into()
        } else {
            "An open answer with no rubric. Respond to what the learner said; do not give a verdict."
                .into()
        };
    };
    match judgment.verdict {
        Verdict::Correct => "Judged correct against your rubric. One correct answer is evidence, \
            not mastery: build on the learner's reasoning."
            .into(),
        Verdict::Partial | Verdict::Incorrect => format!(
            "Judged {} against your rubric. Missing or wrong: {}. Repair that specific idea, \
             then check it again.",
            if judgment.verdict == Verdict::Partial {
                "partial"
            } else {
                "incorrect"
            },
            judgment.missing.as_deref().unwrap_or("(not named)")
        ),
        Verdict::Unclear if judgment.follow_up.is_some() => {
            "Unclear, so no verdict: SAIL asked the learner the follow-up question that comes next."
                .into()
        }
        Verdict::Unclear => "Still unclear after a follow-up, so no verdict. Explain the idea \
            another way and check it again."
            .into(),
        Verdict::CouldNotJudge => "SAIL could not judge this answer. Do not guess a verdict: \
            respond to the learner's reasoning and ask again if you need to."
            .into(),
    }
}

/// The text a model sees as a question's result.
pub fn answer_result(
    spec: &QuestionSpec,
    answer: &Answer,
    grade: Grade,
    judgment: Option<&Judgment>,
) -> String {
    let chosen: Vec<&str> = answer
        .choices
        .iter()
        .filter_map(|id| spec.options.iter().find(|o| &o.id == id))
        .map(|o| o.text.as_str())
        .collect();
    let mut result = json!({
        "learner_answer": {
            "chosen": chosen,
            "text": answer.text,
            "other": answer.other,
            "dont_know": answer.dont_know,
            "confidence_1_to_5": answer.confidence,
            "confidence_pct": answer.confidence_pct,
            "thinking": answer.thinking,
        },
        "grade": grade,
        "note": answer_note(spec, grade, judgment),
    });
    if let Some(judgment) = judgment {
        result["judgment"] = json!({
            "verdict": judgment.verdict,
            "missing": judgment.missing,
            "feedback": judgment.feedback,
        });
    }
    result.to_string()
}

pub fn to_llm_messages(system_prompt: &str, events: &[Envelope]) -> Vec<Message> {
    let mut out = vec![Message::text(Role::System, system_prompt)];
    // Calls of the latest assistant message that have no result yet, in order.
    let mut pending: Vec<String> = Vec::new();
    let mut questions: Vec<(String, &QuestionSpec)> = Vec::new();
    let mut problems: Vec<(String, String, &super::practice::ProblemSpec)> = Vec::new();
    // Judgments are stored after the answer; the tutor sees them with it.
    let judgments: Vec<(&str, &Judgment)> = events
        .iter()
        .filter_map(|e| match &e.event {
            LearningEvent::AnswerJudged { call_id, judgment } => Some((call_id.as_str(), judgment)),
            _ => None,
        })
        .collect();

    fn flush(out: &mut Vec<Message>, pending: &mut Vec<String>) {
        for id in pending.drain(..) {
            out.push(tool_message(&id, INTERRUPTED.to_string()));
        }
    }

    // The latest compaction (#252) replaces history through `up_to_seq` with its digest. The
    // compaction event itself is appended later, so the history it keeps (events after the
    // cut) is replayed here, after the digest, rather than discarded.
    let compaction = events.iter().rev().find_map(|e| match &e.event {
        LearningEvent::ContextCompacted { up_to_seq, digest } => Some((*up_to_seq, digest)),
        _ => None,
    });
    let mut digested = false;
    let start_after_cut = |out: &mut Vec<Message>, pending: &mut Vec<String>| {
        if let Some((up_to_seq, digest)) = compaction {
            out.clear();
            pending.clear();
            out.push(Message::text(Role::System, system_prompt));
            out.push(Message::text(
                Role::User,
                format!(
                    "Earlier parts of this lesson (through event {up_to_seq}) are represented by this digest, not repeated:\n{digest}"
                ),
            ));
        }
    };
    // Questions and problems asked before the cut: their calls are no longer in the prompt,
    // so an answer that arrives after the cut is passed on as plain learner text.
    let mut before_cut: Vec<String> = Vec::new();

    for envelope in events {
        if let Some((up_to_seq, _)) = compaction
            && !digested
            && envelope.seq > up_to_seq
        {
            digested = true;
            start_after_cut(&mut out, &mut pending);
        }
        let pre_cut = compaction.is_some_and(|(up_to_seq, _)| envelope.seq <= up_to_seq);
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
            LearningEvent::QuestionAsked { call_id, spec, .. } => {
                if pre_cut {
                    before_cut.push(call_id.clone());
                }
                questions.push((call_id.clone(), spec));
            }
            LearningEvent::ProblemAsked {
                call_id,
                problem_id,
                spec,
            } => {
                if pre_cut {
                    before_cut.push(call_id.clone());
                }
                problems.push((call_id.clone(), problem_id.clone(), spec));
            }
            LearningEvent::PracticeAttempted { problem_id, text } => {
                if let Some(i) = problems.iter().position(|(_, id, _)| id == problem_id)
                    && !pending.iter().any(|id| id == &problems[i].0)
                    && before_cut.contains(&problems[i].0)
                {
                    out.push(Message::text(
                        Role::User,
                        json!({
                            "learner_attempt_for_problem": problem_id,
                            "learner_attempt": text,
                            "note": "This problem was posed before the digest. Practice evidence only. Evaluate it and call `conclude` with solved, solved_with_hints or not_solved.",
                        })
                        .to_string(),
                    ));
                } else if let Some(i) = problems.iter().position(|(_, id, _)| id == problem_id)
                    && pending.iter().any(|id| id == &problems[i].0)
                {
                    let call_id = problems[i].0.clone();
                    pending.retain(|id| id != &call_id);
                    out.push(tool_message(
                        &call_id,
                        json!({
                            "learner_attempt": text,
                            "note": "Practice evidence only. Evaluate it and call `conclude` with solved, solved_with_hints or not_solved; the solution is then revealed.",
                        })
                        .to_string(),
                    ));
                }
            }
            LearningEvent::HintRequested { .. }
            | LearningEvent::PracticeConcluded { .. }
            | LearningEvent::PracticeReviewScheduled { .. }
            | LearningEvent::PracticeReviewCancelled
            | LearningEvent::PracticeStarted { .. } => {}
            LearningEvent::DiagramShown { .. } => {}
            LearningEvent::DiagramRenderFailed { call_id, message } => {
                out.push(Message::text(
                    Role::User,
                    format!(
                        "Tool `show_diagram` failed in the browser for call {call_id}: {message}. \
                         Use a simpler diagram or explain it in words."
                    ),
                ));
            }
            // Applied at the cut point above; older compactions are superseded.
            LearningEvent::ContextCompacted { .. } => {}
            LearningEvent::Answered {
                call_id,
                answer,
                grade,
            } => {
                if let Some(i) = pending.iter().position(|id| id == call_id)
                    && let Some((_, spec)) = questions.iter().find(|(id, _)| id == call_id)
                {
                    pending.remove(i);
                    let judgment = judgments
                        .iter()
                        .find(|(id, _)| id == call_id)
                        .map(|(_, j)| *j);
                    out.push(tool_message(
                        call_id,
                        answer_result(spec, answer, *grade, judgment),
                    ));
                } else if before_cut.contains(call_id)
                    && let Some((_, spec)) = questions.iter().find(|(id, _)| id == call_id)
                {
                    let judgment = judgments
                        .iter()
                        .find(|(id, _)| id == call_id)
                        .map(|(_, j)| *j);
                    out.push(Message::text(
                        Role::User,
                        format!(
                            "Answer to a question asked before the digest: {}",
                            answer_result(spec, answer, *grade, judgment)
                        ),
                    ));
                }
            }
            LearningEvent::AnswerJudged { .. }
            | LearningEvent::SessionRenamed { .. }
            | LearningEvent::GraphContextAttached { .. }
            | LearningEvent::QuestionDismissed { .. }
            | LearningEvent::ProbeDone { .. }
            | LearningEvent::PlanUpdated { .. }
            | LearningEvent::ConceptPresented { .. }
            | LearningEvent::TurnSettled { .. }
            | LearningEvent::Paused
            | LearningEvent::Resumed => {}
        }
    }
    if compaction.is_some() && !digested {
        start_after_cut(&mut out, &mut pending);
    }
    // At the end, an unanswered question or problem keeps its placeholder; anything else
    // was cut off.
    for id in pending.drain(..) {
        let asked = questions.iter().any(|(q, _)| *q == id);
        let practising = problems.iter().any(|(call, _, _)| *call == id);
        out.push(tool_message(
            &id,
            if asked {
                AWAITING.to_string()
            } else if practising {
                "Awaiting the learner's practice attempt.".to_string()
            } else {
                INTERRUPTED.to_string()
            },
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
    window_within(messages, MAX_BYTES)
}

/// [`window`] with a smaller history budget, for a model whose context window is smaller
/// than Mind's request bound (the turn halves it after a context-window refusal).
pub fn window_within(messages: Vec<Message>, max_bytes: usize) -> Vec<Message> {
    let max_bytes = max_bytes.min(MAX_BYTES);
    let total: usize = messages.iter().map(size).sum();
    if messages.len() <= MAX_MESSAGES && total <= max_bytes {
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
        if count > MAX_MESSAGES || bytes > max_bytes {
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

/// Approximate prompt tokens for one stored event. Conservative (JSON length / 4).
pub fn event_tokens(envelope: &Envelope) -> u64 {
    (serde_json::to_string(&envelope.event)
        .map(|s| s.len())
        .unwrap_or(0) as u64)
        .div_ceil(4)
}

/// The latest compaction's `up_to_seq` (0 when none) and its digest's approximate tokens.
pub fn latest_compaction(events: &[Envelope]) -> (u64, u64) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.event {
            LearningEvent::ContextCompacted { up_to_seq, digest } => {
                Some((*up_to_seq, approx_text_tokens(digest)))
            }
            _ => None,
        })
        .unwrap_or((0, 0))
}

/// Approximate tokens of a piece of prompt text (bytes / 4, rounded up).
pub fn approx_text_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// Picks a compaction cut for a long session (#252). Returns the `up_to_seq` to replace.
///
/// The cut is always immediately before an input event (the learner's answer, message or
/// practice attempt), so an assistant tool call is never separated from its result. It
/// measures what the prompt replays: the latest digest plus the events after its cut, so the
/// kept history counts and a fresh compaction is not cut again at once. `None` when that fits.
pub fn compaction_cut(events: &[Envelope], budget_tokens: u64) -> Option<u64> {
    // The prompt replays the latest digest plus every event after its cut, so measure that.
    let (after, digest_tokens) = latest_compaction(events);
    let recent: Vec<&Envelope> = events
        .iter()
        .filter(|e| e.seq > after && !matches!(e.event, LearningEvent::ContextCompacted { .. }))
        .collect();
    let total: u64 = digest_tokens + recent.iter().map(|e| event_tokens(e)).sum::<u64>();
    if total <= budget_tokens {
        return None;
    }
    // Keep roughly the newest half; find the first input boundary at or after that point.
    let target = budget_tokens / 2;
    let mut acc = 0u64;
    for i in (0..recent.len()).rev() {
        acc = acc.saturating_add(event_tokens(recent[i]));
        if acc > target {
            let boundary = (i..recent.len()).find(|&j| recent[j].event.is_input())?;
            let up_to = if boundary == 0 {
                0
            } else {
                recent[boundary - 1].seq
            };
            return Some(up_to);
        }
    }
    None
}

/// A deterministic summary that replaces compacted history (#252): goal, plan, progress,
/// the last answers and the last explanation. No model call, no clock.
pub fn compact_digest(events: &[Envelope], up_to_seq: u64) -> String {
    let kept: Vec<Envelope> = events
        .iter()
        .filter(|e| e.seq <= up_to_seq)
        .cloned()
        .collect();
    let view = super::view::project_view("", &kept);
    let mut out = String::from("Compacted lesson state:\n");
    out.push_str(&format!("- goal: {}\n", view.goal));
    if let Some(plan) = &view.plan {
        let titles: Vec<String> = plan
            .order
            .iter()
            .map(|id| {
                let title = plan
                    .nodes
                    .iter()
                    .find(|n| &n.id == id)
                    .map(|n| n.title.as_str())
                    .unwrap_or(id);
                format!("{id} ({title})")
            })
            .collect();
        out.push_str(&format!(
            "- plan revision {}: teaching order {}\n",
            plan.revision,
            titles.join(" -> ")
        ));
        out.push_str(&format!(
            "- visited: {}; current: {}; upcoming: {}\n",
            view.progress.visited.join(", "),
            view.progress.current.as_deref().unwrap_or("none"),
            view.progress.upcoming.join(", ")
        ));
    }
    if let Some(probe) = &view.probe {
        out.push_str(&format!("- assessment: {}\n", probe.summary));
    }
    // Last few learner answers and the verdict the tutor already saw.
    let mut answers = Vec::new();
    for envelope in kept.iter().rev() {
        if let LearningEvent::Answered {
            call_id,
            answer,
            grade,
        } = &envelope.event
        {
            let prompt = super::view::question_spec(&kept, call_id)
                .map(|q| q.prompt.clone())
                .unwrap_or_default();
            let judgment = super::view::judgment_of(&kept, call_id)
                .map(|j| format!("{:?}", j.verdict))
                .unwrap_or_default();
            let text = answer
                .text
                .clone()
                .unwrap_or_else(|| answer.choices.join(", "));
            answers.push(format!(
                "  - Q: {prompt} | A: {text} | grade: {grade:?} {judgment}"
            ));
            if answers.len() == 3 {
                break;
            }
        }
    }
    if !answers.is_empty() {
        out.push_str("- last answers:\n");
        out.push_str(&answers.join("\n"));
        out.push('\n');
    }
    if let Some(last_explain) = kept.iter().rev().find_map(|e| match &e.event {
        LearningEvent::ConceptPresented {
            kind: super::events::PresentKind::Explain,
            text,
            ..
        } => Some(text),
        _ => None,
    }) {
        let short: String = last_explain.chars().take(400).collect();
        out.push_str(&format!("- last explanation: {short}\n"));
    }
    out
}
