//! Night Shift voice tools (round 27): add an errand, ask what happened overnight (with a diff
//! excerpt, which is how "Ask about this" works), and ship, redo or drop a result.
//!
//! Shipping by voice needs one yes/no: without `confirmed: true` the tool only returns the
//! question to ask. A shipped result is a **draft** PR; nothing here merges.

use sail_forge::nightshift::{Errand, ErrandId, ErrandSource, ErrandState};
use serde_json::{Value, json};

use crate::nightshift::{ApproveError, NightShift};

/// Every Night Shift tool this module answers, checked by the registry tests.
#[cfg(test)]
pub const TOOL_IDS: [&str; 3] = ["nightshift:add", "nightshift:status", "nightshift:review"];

/// Longest diff excerpt returned to the model, in characters. Fixed a priori: enough for the
/// model to discuss a small change, bounded so one tool result can't flood the context.
const DIFF_EXCERPT_CHARS: usize = 6_000;

pub fn execute(
    service: &std::sync::Arc<NightShift>,
    tool: &str,
    input: &Value,
) -> Result<Value, String> {
    match tool {
        "nightshift:add" => {
            let ask = input["ask"].as_str().unwrap_or_default();
            let errand = service.add(ask, ErrandSource::Voice)?;
            Ok(json!({
                "errand_id": errand.id.as_str(),
                "state": "shaping",
                "say": "Got it. I'll draft a work order for tonight; you can approve it in Night Shift.",
            }))
        }
        "nightshift:status" => match input["errand_id"].as_str().filter(|id| !id.is_empty()) {
            Some(raw) => {
                let id = parse(raw)?;
                let errand = service.store().get(&id).ok_or("no such errand")?;
                let mut out = brief(&errand);
                if let Some(patch) = service.diff(&id) {
                    let excerpt: String = patch.chars().take(DIFF_EXCERPT_CHARS).collect();
                    out["diff_excerpt"] = json!(excerpt);
                    out["diff_truncated"] = json!(patch.chars().count() > DIFF_EXCERPT_CHARS);
                }
                Ok(out)
            }
            None => {
                let errands: Vec<Value> = service
                    .store()
                    .list()
                    .iter()
                    .filter(|errand| !matches!(errand.state, ErrandState::Dropped { .. }))
                    .take(10)
                    .map(brief)
                    .collect();
                Ok(json!({"errands": errands}))
            }
        },
        "nightshift:review" => {
            let id = parse(input["errand_id"].as_str().unwrap_or_default())?;
            let action = input["action"].as_str().unwrap_or_default();
            let note = input["note"].as_str().unwrap_or_default();
            match action {
                "ship" => {
                    let errand = service.store().get(&id).ok_or("no such errand")?;
                    if input["confirmed"].as_bool() != Some(true) {
                        return Ok(json!({
                            "needs_confirmation": true,
                            "ask_owner": format!("Ship \"{}\" as a draft pull request?", title(&errand)),
                        }));
                    }
                    let errand = service.ship(&id)?;
                    Ok(
                        json!({"errand": brief(&errand), "say": "Shipping it as a draft pull request."}),
                    )
                }
                "redo" => {
                    let errand = service.redo(&id, note)?;
                    Ok(
                        json!({"errand": brief(&errand), "say": "Queued again for tonight with your note."}),
                    )
                }
                "drop" => {
                    let errand = service.drop_errand(&id)?;
                    Ok(json!({"errand": brief(&errand), "say": "Dropped."}))
                }
                "approve" => match service.approve(&id, None) {
                    Ok(errand) => {
                        Ok(json!({"errand": brief(&errand), "say": "Approved for tonight."}))
                    }
                    Err(ApproveError::Invalid(reasons)) => Err(reasons.join("; ")),
                    Err(ApproveError::Refused(message)) => Err(message),
                },
                _ => Err("action must be ship, redo, drop or approve".into()),
            }
        }
        other => Err(format!("unknown Night Shift tool {other}")),
    }
}

fn parse(raw: &str) -> Result<ErrandId, String> {
    ErrandId::parse(raw.trim()).ok_or_else(|| "errand_id must look like e-0a1b2c3d4e5f".into())
}

fn title(errand: &Errand) -> String {
    match &errand.state {
        ErrandState::Proposed { order }
        | ErrandState::Queued { order, .. }
        | ErrandState::Working { order, .. }
        | ErrandState::Ready { order, .. }
        | ErrandState::NeedsYou { order, .. }
        | ErrandState::Failed { order, .. }
        | ErrandState::Shipping { order, .. }
        | ErrandState::Shipped { order, .. } => order.title.clone(),
        _ => errand.ask.chars().take(80).collect(),
    }
}

/// What the model needs to talk about one errand. Model-written text is labelled.
fn brief(errand: &Errand) -> Value {
    let state = serde_json::to_value(&errand.state).unwrap_or(Value::Null);
    let mut out = json!({
        "errand_id": errand.id.as_str(),
        "title": title(errand),
        "ask": errand.ask,
        "state": state["state"],
    });
    match &errand.state {
        ErrandState::Ready { result, .. }
        | ErrandState::NeedsYou { result, .. }
        | ErrandState::Shipping { result, .. }
        | ErrandState::Shipped { result, .. } => {
            out["summary"] = json!(result.summary);
            out["ran_on"] = json!(result.ran_on);
            out["files"] = json!(result.files);
            if let Some(note) = &result.model_note {
                out["model_written_note"] = json!(note);
            }
        }
        ErrandState::Failed { reason, .. } => out["reason"] = json!(reason),
        ErrandState::Unshapeable { reasons } => out["reasons"] = json!(reasons),
        _ => {}
    }
    match &errand.state {
        ErrandState::NeedsYou { question, .. } => out["question"] = json!(question),
        ErrandState::Shipped { pr_url, .. } => out["pr_url"] = json!(pr_url),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errand_ids_are_validated_before_use() {
        assert!(parse("e-0a1b2c3d4e5f").is_ok());
        assert!(parse(" e-0a1b2c3d4e5f ").is_ok());
        assert!(parse("../../etc").is_err());
        assert!(parse("").is_err());
    }
}
