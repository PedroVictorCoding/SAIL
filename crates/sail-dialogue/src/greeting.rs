//! Deterministic arrival greeting.
//!
//! Composed from held announcements and the return briefing only: no model call and no
//! invented clause. Every clause is taken verbatim from a data field; the only generated
//! text is the opening and the relative-time wording, both fixed templates.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use sail_pulse::attention::{AttentionItem, Delivery};

use crate::briefing::{BriefingEvent, ReturnBriefing};

/// Greeting language. serde: `"en" | "pt_pt"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    En,
    PtPt,
}

/// What to say, and which held items it mentioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Greeting {
    pub spoken: String,
    pub item_ids: Vec<String>,
}

/// Longest spoken greeting, in characters (fixed a priori: two short sentences).
pub const MAX_SPOKEN_CHARS: usize = 220;

/// Longest data clause, in characters, trimmed at a word boundary.
pub const MAX_CLAUSE_CHARS: usize = 80;

/// An event is worth mentioning only when it is due within this many hours
/// (fixed a priori: today's and tonight's events, not next week's).
const EVENT_WINDOW_HOURS: i64 = 12;

/// Compose the greeting. Pure. `local_hour` is the owner's local hour (0..=23) at `now`.
/// `away_for_secs` is accepted for the caller's context but does not change the wording:
/// the opening already says the owner is back.
pub fn compose(
    briefing: &ReturnBriefing,
    held: &[AttentionItem],
    _away_for_secs: u64,
    now: DateTime<Utc>,
    local_hour: u32,
    lang: Lang,
) -> Greeting {
    let opening = opening(local_hour, lang);
    let mut item_ids = Vec::new();
    let mut clauses: Vec<String> = Vec::new();

    if let Some(item) = best_held(held)
        && let Some(clause) = clause_text(&item.signal.title)
    {
        clauses.push(clause);
        item_ids.push(item.id.clone());
    }
    if let Some(clause) = event_clause(&briefing.events_to_watch, now, lang) {
        clauses.push(clause);
    }
    if let Some(continuity) = &briefing.project_continuity
        && let Some(clause) = clause_text(&continuity.suggested_action)
    {
        clauses.push(clause);
    }
    clauses.truncate(2);

    let spoken = match clauses.len() {
        0 => opening.to_owned(),
        1 => sentence(opening, &clauses[0]),
        _ => {
            let joiner = match lang {
                Lang::En => ", and ",
                Lang::PtPt => ", e ",
            };
            let mut text = format!("{} {}{}{}", opening, clauses[0], joiner, clauses[1]);
            if !terminal(&text) {
                text.push('.');
            }
            text
        }
    };
    let spoken = fit(spoken, opening, &clauses);

    Greeting { spoken, item_ids }
}

fn opening(local_hour: u32, lang: Lang) -> &'static str {
    match (lang, local_hour) {
        (Lang::En, 5..=11) => "Good morning.",
        (Lang::En, 12..=19) => "Welcome back.",
        (Lang::En, _) => "Good evening.",
        (Lang::PtPt, 5..=11) => "Bom dia.",
        (Lang::PtPt, 12..=19) => "Bem-vindo de volta.",
        (Lang::PtPt, _) => "Boa noite.",
    }
}

/// Highest-score held `Interrupt` or `Suggest`; newest first on ties.
fn best_held(held: &[AttentionItem]) -> Option<&AttentionItem> {
    held.iter()
        .filter(|item| {
            matches!(
                item.assessment.delivery,
                Delivery::Interrupt | Delivery::Suggest
            )
        })
        .max_by(|a, b| {
            a.assessment
                .score
                .total_cmp(&b.assessment.score)
                .then(a.created_at.cmp(&b.created_at))
        })
}

fn event_clause(events: &[BriefingEvent], now: DateTime<Utc>, lang: Lang) -> Option<String> {
    let event = events
        .iter()
        .filter(|event| {
            event.due_at >= now && event.due_at <= now + Duration::hours(EVENT_WINDOW_HOURS)
        })
        .min_by(|a, b| a.due_at.cmp(&b.due_at).then(a.id.cmp(&b.id)))?;
    let title = clause_text(&event.title)?;
    let relative = relative(due_seconds(event.due_at, now), lang);
    Some(match lang {
        Lang::En => format!("{title} is due {relative}"),
        Lang::PtPt => format!("{title} é {relative}"),
    })
}

fn due_seconds(due: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    due.signed_duration_since(now).num_seconds().max(0)
}

fn relative(secs: i64, lang: Lang) -> String {
    if secs < 3_600 {
        let minutes = (secs + 59) / 60;
        let minutes = minutes.max(1);
        return match lang {
            Lang::En => format!("in {minutes} {}", plural(minutes, "minute", "minutes")),
            Lang::PtPt => format!("daqui a {minutes} {}", plural(minutes, "minuto", "minutos")),
        };
    }
    let hours = (secs + 3_599) / 3_600;
    match lang {
        Lang::En => format!("in {hours} {}", plural(hours, "hour", "hours")),
        Lang::PtPt => format!("daqui a {hours} {}", plural(hours, "hora", "horas")),
    }
}

fn plural<'a>(count: i64, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

/// Trim to [`MAX_CLAUSE_CHARS`] at a word boundary. Empty or whitespace-only fields give
/// no clause.
fn clause_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= MAX_CLAUSE_CHARS {
        return Some(text.to_owned());
    }
    let cut: String = chars[..MAX_CLAUSE_CHARS].iter().collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(position) => cut[..position].trim_end(),
        None => cut.trim_end(),
    };
    if cut.is_empty() {
        None
    } else {
        Some(cut.to_owned())
    }
}

fn terminal(text: &str) -> bool {
    text.ends_with(['.', '!', '?'])
}

fn sentence(opening: &str, clause: &str) -> String {
    if terminal(clause) {
        format!("{opening} {clause}")
    } else {
        format!("{opening} {clause}.")
    }
}

/// Keep `spoken` within [`MAX_SPOKEN_CHARS`]. The normal composition is well below the
/// cap; this is a defensive guard if wider clauses are ever allowed.
fn fit(spoken: String, opening: &str, clauses: &[String]) -> String {
    if spoken.chars().count() <= MAX_SPOKEN_CHARS {
        return spoken;
    }
    if let Some(first) = clauses.first() {
        let shorter = sentence(opening, first);
        if shorter.chars().count() <= MAX_SPOKEN_CHARS {
            return shorter;
        }
    }
    let mut cut: String = spoken.chars().take(MAX_SPOKEN_CHARS).collect();
    if let Some(position) = cut.rfind(char::is_whitespace) {
        cut.truncate(position);
    }
    cut.trim_end().to_owned()
}
