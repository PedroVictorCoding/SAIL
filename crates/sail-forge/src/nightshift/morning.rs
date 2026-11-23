//! K4: the deterministic morning report.
//!
//! No model call composes [`MorningReport::spoken`] or the per-item lines. Counts follow
//! the plan order and the errand states at the end of the night.

use std::collections::BTreeMap;

use super::{
    Errand, ErrandState, MorningItem, MorningOutcome, MorningReport, NightItem, NightPlan,
    RanOnKind,
};

const HEAVY_UNAVAILABLE: &str = "heavy_unavailable";

/// Shorten a title to at most six words and remove anything that looks like a file path.
fn short_title(title: &str) -> String {
    let words: Vec<&str> = title
        .split_whitespace()
        .filter(|word| !word.contains('/') && !word.contains('\\'))
        .take(6)
        .collect();
    if words.is_empty() {
        "a job".into()
    } else {
        words.join(" ")
    }
}

/// The approved order for a state, when the state still carries one.
fn order_title(state: &ErrandState) -> Option<&str> {
    match state {
        ErrandState::Proposed { order }
        | ErrandState::Queued { order, .. }
        | ErrandState::Working { order, .. }
        | ErrandState::Ready { order, .. }
        | ErrandState::NeedsYou { order, .. }
        | ErrandState::Failed { order, .. }
        | ErrandState::Shipping { order, .. }
        | ErrandState::Shipped { order, .. } => Some(&order.title),
        ErrandState::Shaping | ErrandState::Unshapeable { .. } | ErrandState::Dropped { .. } => {
            None
        }
    }
}

fn outcome_of(state: &ErrandState) -> MorningOutcome {
    match state {
        ErrandState::Ready { .. } | ErrandState::Shipping { .. } | ErrandState::Shipped { .. } => {
            MorningOutcome::Ready
        }
        ErrandState::NeedsYou { .. } => MorningOutcome::NeedsYou,
        ErrandState::Failed { .. } => MorningOutcome::Failed,
        _ => MorningOutcome::NotReached,
    }
}

fn pooled_line(ran_on: &super::RanOn, rest: &str) -> String {
    if ran_on.kind == RanOnKind::Pooled {
        let prefix = format!("Ran on the big model (host + {}).", ran_on.name);
        if rest.is_empty() {
            prefix
        } else {
            format!("{prefix} {rest}")
        }
    } else {
        rest.to_string()
    }
}

fn line_of(state: &ErrandState, halted: Option<&str>) -> String {
    match state {
        ErrandState::Ready { result, .. }
        | ErrandState::Shipping { result, .. }
        | ErrandState::Shipped { result, .. } => pooled_line(&result.ran_on, &result.summary),
        ErrandState::NeedsYou {
            result, question, ..
        } => {
            let body = format!("{} Needs you: {question}", result.summary);
            pooled_line(&result.ran_on, body.trim())
        }
        ErrandState::Failed { reason, result, .. } => match result {
            Some(result) if !result.summary.is_empty() => {
                let body = format!("{} {reason}", result.summary);
                pooled_line(&result.ran_on, &body)
            }
            Some(result) => pooled_line(&result.ran_on, reason),
            None => reason.clone(),
        },
        ErrandState::Queued { .. } | ErrandState::Working { .. } => {
            if halted == Some(HEAVY_UNAVAILABLE) {
                "Waited — the PC wasn't lending.".into()
            } else if halted.is_some() {
                "Not reached: the night stopped early.".into()
            } else {
                "Not reached before the morning.".into()
            }
        }
        _ => "Not reached before the morning.".into(),
    }
}

/// Compose the morning report. Pure: plan order, errand states and issue titles are the
/// only inputs, and the spoken text is a fixed template of at most two sentences.
pub fn compose_morning(
    date: &str,
    plan: &NightPlan,
    errands: &[Errand],
    issue_titles: &BTreeMap<u64, String>,
    issue_results: &BTreeMap<u64, (MorningOutcome, String)>,
    halted: Option<String>,
) -> MorningReport {
    let by_id: BTreeMap<&str, &Errand> = errands
        .iter()
        .map(|errand| (errand.id.as_str(), errand))
        .collect();
    let mut items = Vec::new();
    let mut ready = 0u32;
    let mut needs_you = 0u32;
    let mut failed = 0u32;
    let mut not_reached = 0u32;

    for item in &plan.items {
        let (title, outcome, line) = match item {
            NightItem::Errand { id } => match by_id.get(id.as_str()) {
                Some(errand) => {
                    let title = order_title(&errand.state)
                        .map(sanitize_title)
                        .unwrap_or_else(|| short_title(&errand.ask));
                    (
                        title,
                        outcome_of(&errand.state),
                        line_of(&errand.state, halted.as_deref()),
                    )
                }
                None => (
                    "an errand".into(),
                    MorningOutcome::NotReached,
                    "Not reached before the morning.".into(),
                ),
            },
            NightItem::Issue { number } => {
                let title = issue_titles
                    .get(number)
                    .map(|title| sanitize_title(title))
                    .unwrap_or_else(|| format!("Issue #{number}"));
                match issue_results.get(number) {
                    Some((outcome, line)) => (title, *outcome, line.clone()),
                    None => (
                        title,
                        MorningOutcome::NotReached,
                        "Not reached before the morning.".into(),
                    ),
                }
            }
        };
        match outcome {
            MorningOutcome::Ready => ready += 1,
            MorningOutcome::NeedsYou => needs_you += 1,
            MorningOutcome::Failed => failed += 1,
            MorningOutcome::NotReached => not_reached += 1,
        }
        items.push(MorningItem {
            item: item.clone(),
            title,
            outcome,
            line,
        });
    }

    let pooled_worker = plan.items.iter().find_map(|item| {
        let NightItem::Errand { id } = item else {
            return None;
        };
        let errand = by_id.get(id.as_str())?;
        let result = match &errand.state {
            ErrandState::Ready { result, .. }
            | ErrandState::Shipping { result, .. }
            | ErrandState::Shipped { result, .. }
            | ErrandState::NeedsYou { result, .. } => Some(result),
            ErrandState::Failed { result, .. } => result.as_ref(),
            _ => None,
        }?;
        (result.ran_on.kind == RanOnKind::Pooled).then(|| result.ran_on.name.as_str())
    });

    let total = items.len() as u32;
    let ran = ready + needs_you + failed;
    let spoken = if ran == 0 {
        if halted.as_deref() == Some(HEAVY_UNAVAILABLE) && !items.is_empty() {
            "Waited — the PC wasn't lending.".into()
        } else {
            String::new()
        }
    } else {
        compose_spoken(
            total,
            ready,
            needs_you,
            failed,
            &items,
            halted.as_deref(),
            pooled_worker,
        )
    };

    MorningReport {
        date: date.to_string(),
        ready,
        needs_you,
        failed,
        not_reached,
        items,
        spoken,
        halted,
    }
}

fn sanitize_title(title: &str) -> String {
    let shortened = short_title(title);
    if shortened.is_empty() {
        "a job".into()
    } else {
        shortened
    }
}

fn compose_spoken(
    total: u32,
    ready: u32,
    needs_you: u32,
    failed: u32,
    items: &[MorningItem],
    halted: Option<&str>,
    pooled_worker: Option<&str>,
) -> String {
    let mut sentences: Vec<String> = Vec::new();
    let review_phrase = match ready {
        0 => String::new(),
        1 => "one is ready to review".into(),
        2 => "both are ready to review".into(),
        _ => format!("{ready} are ready to review"),
    };
    let first = format!("Overnight I finished {ready} of {total} jobs");
    sentences.push(if review_phrase.is_empty() {
        format!("{first}.")
    } else {
        format!("{first}; {review_phrase}.")
    });

    if let Some(halted) = halted {
        if halted == HEAVY_UNAVAILABLE {
            sentences.push("Waited — the PC wasn't lending.".into());
        } else {
            sentences.push(format!("The night stopped early: {}.", one_line(halted)));
        }
    } else if needs_you > 0 {
        let title = items
            .iter()
            .find(|item| item.outcome == MorningOutcome::NeedsYou)
            .map(|item| short_title(&item.title))
            .unwrap_or_else(|| "a job".into());
        if needs_you == 1 {
            sentences.push(format!("The {title} needs a decision from you."));
        } else {
            sentences.push(format!("{needs_you} jobs need a decision from you."));
        }
    } else if failed > 0 {
        sentences.push(if failed == 1 {
            "1 job failed; the details are in the review.".into()
        } else {
            format!("{failed} jobs failed; the details are in the review.")
        });
    } else if let Some(worker) = pooled_worker {
        sentences.push(format!("Ran on the big model (host + {worker})."));
    }
    sentences.truncate(2);
    sentences.join(" ")
}

fn one_line(text: &str) -> String {
    let cleaned: Vec<&str> = text.split_whitespace().take(24).collect();
    cleaned.join(" ")
}
