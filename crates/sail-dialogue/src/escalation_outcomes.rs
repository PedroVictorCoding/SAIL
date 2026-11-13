//! Automatic escalation outcomes (#450): measure whether a stronger model's answer helped,
//! so routing learns without the owner rating every answer.
//!
//! Two checks, both deterministic and cheap:
//!
//! - **`adopted_in_reply`.** When `escalate_reasoning` hands an answer to the local model
//!   during a turn, the turn's final reply is compared with it. If the reply reuses at least
//!   [`ADOPTED_SHARE`] of the answer's content words, the answer helped (`Improved`). If it
//!   reuses less than [`IGNORED_SHARE`], the model went its own way (`NoChange`). In between,
//!   or for answers too short to judge, nothing is recorded.
//! - **`forge_tests`.** An escalation the model tied to a Forge job (`forge_job_id`) is
//!   filed under correlation id `forge:<job id>`. When that job reaches review or completes
//!   (its tests passed), the escalation is `Improved`; when it ends blocked, `NoChange`.
//!
//! The owner's own feedback always wins (`EscalationService::observe_outcome`).

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sail_mind::escalation::Outcome;

/// Share of the answer's content words the reply must reuse to count as adopted.
pub const ADOPTED_SHARE: f32 = 0.30;
/// Below this share the answer was not used.
pub const IGNORED_SHARE: f32 = 0.08;
/// Answers with fewer distinct content words are too short to judge.
pub const MIN_CONTENT_WORDS: usize = 6;
/// A turn's pending answers are forgotten after this.
const PENDING_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_TURNS: usize = 64;

struct Pending {
    job_id: String,
    answer: String,
    at: Instant,
}

fn pending() -> &'static Mutex<HashMap<String, Vec<Pending>>> {
    static PENDING: OnceLock<Mutex<HashMap<String, Vec<Pending>>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `execute_escalation` gave `answer` (job `job_id`) to the model in turn `turn`.
pub fn note_answer(turn: &str, job_id: &str, answer: &str) {
    let mut map = pending().lock().unwrap_or_else(|p| p.into_inner());
    map.retain(|_, items| {
        items.retain(|p| p.at.elapsed() < PENDING_TTL);
        !items.is_empty()
    });
    if map.len() >= MAX_TURNS && !map.contains_key(turn) {
        return;
    }
    let items = map.entry(turn.to_owned()).or_default();
    if items.iter().any(|p| p.job_id == job_id) || items.len() >= 4 {
        return;
    }
    items.push(Pending {
        job_id: job_id.to_owned(),
        answer: answer.chars().take(8_000).collect(),
        at: Instant::now(),
    });
}

/// Turn `turn` finished with `reply`: record what the reply shows about each answer.
pub fn settle_turn(turn: &str, reply: &str) {
    let items = {
        let mut map = pending().lock().unwrap_or_else(|p| p.into_inner());
        map.remove(turn).unwrap_or_default()
    };
    if items.is_empty() {
        return;
    }
    let Some(service) = sail_mind::escalation::service::global() else {
        return;
    };
    for item in items {
        let Some(outcome) = adoption(&item.answer, reply) else {
            continue;
        };
        if let Err(error) = service.observe_outcome(&item.job_id, outcome, "adopted_in_reply") {
            tracing::debug!(%error, job = %item.job_id, "adoption outcome not recorded");
        }
    }
}

/// `Improved` when the reply adopted the answer, `NoChange` when it ignored it, `None` when
/// unclear. Pure.
pub fn adoption(answer: &str, reply: &str) -> Option<Outcome> {
    let answer_words = content_words(answer);
    if answer_words.len() < MIN_CONTENT_WORDS {
        return None;
    }
    let reply_words = content_words(reply);
    let shared = answer_words.intersection(&reply_words).count();
    let share = shared as f32 / answer_words.len() as f32;
    if share >= ADOPTED_SHARE {
        Some(Outcome::Improved)
    } else if share < IGNORED_SHARE {
        Some(Outcome::NoChange)
    } else {
        None
    }
}

/// Lowercased words of four or more letters or digits, minus common filler (English and
/// Portuguese, the owner's languages).
fn content_words(text: &str) -> HashSet<String> {
    const FILLER: &[&str] = &[
        "that", "this", "with", "from", "have", "will", "your", "they", "them", "then", "than",
        "there", "their", "what", "when", "which", "would", "could", "should", "about", "into",
        "also", "just", "more", "some", "such", "only", "very", "been", "were", "does", "each",
        "para", "como", "mais", "isso", "esta", "este", "essa", "esse", "pelo", "pela", "sobre",
        "quando", "porque", "também", "muito", "ainda", "pode", "você", "seus", "suas", "entre",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_lowercase)
        .filter(|w| !FILLER.contains(&w.as_str()))
        .collect()
}

/// Correlation id for escalations tied to Forge job `forge_job_id`.
pub fn forge_correlation(forge_job_id: &str) -> String {
    format!("forge:{forge_job_id}")
}

/// What a Forge job's state says about escalations tied to it, once it is settled.
pub fn forge_outcome(state: sail_forge::ForgeTaskState) -> Option<Outcome> {
    use sail_forge::ForgeTaskState as S;
    match state {
        // Review is requested only after the tests passed.
        S::ReviewPending | S::Completed => Some(Outcome::Improved),
        S::Blocked => Some(Outcome::NoChange),
        _ => None,
    }
}

/// Record Forge outcomes for settled jobs (called from the initiative tick). Each Forge job
/// is settled once per process.
pub fn settle_forge_jobs() {
    static SETTLED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let Some(service) = sail_mind::escalation::service::global() else {
        return;
    };
    let Ok(orchestrator) = sail_forge::global_orchestrator() else {
        return;
    };
    let settled = SETTLED.get_or_init(|| Mutex::new(HashSet::new()));
    for job in orchestrator.list_all_jobs() {
        let Some(outcome) = forge_outcome(job.state) else {
            continue;
        };
        {
            let mut seen = settled.lock().unwrap_or_else(|p| p.into_inner());
            if seen.len() > 10_000 {
                seen.clear();
            }
            if !seen.insert(job.job_id.clone()) {
                continue;
            }
        }
        match service.observe_correlated(&forge_correlation(&job.job_id), outcome, "forge_tests") {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!(forge_job = %job.job_id, ?outcome, n, "forge escalation outcome")
            }
            Err(error) => tracing::debug!(%error, "forge escalation outcome not recorded"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = "Use tokio::select with a CancellationToken; drop the JoinHandle \
        only after the child future observed cancellation, otherwise the spawned task leaks \
        its connection pool.";

    #[test]
    fn a_reply_that_reuses_the_answer_adopted_it() {
        let reply = "The stronger model suggests tokio::select with a CancellationToken, and \
            dropping the JoinHandle only after the child future observed cancellation, so the \
            spawned task doesn't leak its connection pool.";
        assert_eq!(adoption(ANSWER, reply), Some(Outcome::Improved));
    }

    #[test]
    fn an_unrelated_reply_ignored_it() {
        let reply = "Here is a recipe for bread: flour, water, salt and yeast.";
        assert_eq!(adoption(ANSWER, reply), Some(Outcome::NoChange));
    }

    #[test]
    fn short_answers_and_middling_overlap_are_not_judged() {
        assert_eq!(adoption("Yes, use it.", "Yes, use it."), None);
        let middling = "Consider a CancellationToken for the spawned task, then think about \
            other approaches like channels, actors, supervisors, timeouts, retries, backoff.";
        assert_eq!(adoption(ANSWER, middling), None);
    }

    #[test]
    fn forge_states_map_to_outcomes() {
        use sail_forge::ForgeTaskState as S;
        assert_eq!(forge_outcome(S::Completed), Some(Outcome::Improved));
        assert_eq!(forge_outcome(S::ReviewPending), Some(Outcome::Improved));
        assert_eq!(forge_outcome(S::Blocked), Some(Outcome::NoChange));
        assert_eq!(forge_outcome(S::Testing), None);
        assert_eq!(forge_correlation("j1"), "forge:j1");
    }

    #[test]
    fn pending_answers_are_bounded_per_turn() {
        let turn = "turn-bounded-test";
        for i in 0..10 {
            note_answer(turn, &format!("job-{i}"), ANSWER);
        }
        let count = pending().lock().unwrap().get(turn).map_or(0, Vec::len);
        assert_eq!(count, 4);
        settle_turn(turn, "anything");
        assert!(!pending().lock().unwrap().contains_key(turn));
    }
}
