//! Continuity outcomes become Pulse signals with the expected delivery.
use chrono::{DateTime, Duration, Utc};
use sail_discovery::tasks::{TaskEnd, TaskOutcome, task_signals};
use sail_pulse::attention::{
    AttentionContext, AttentionSettings, Delivery, MAX_BODY_CHARS, MAX_TITLE_CHARS, Provenance,
    Source, score,
};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn outcome(id: &str, title: &str, end: TaskEnd, ended_at: DateTime<Utc>) -> TaskOutcome {
    TaskOutcome {
        id: id.into(),
        title: title.into(),
        owner: "forge".into(),
        end,
        ended_at,
        summary: "2 tests failed in sail-flow.".into(),
    }
}

fn context(now: DateTime<Utc>) -> AttentionContext {
    AttentionContext {
        now,
        quiet_hours: false,
        focus_until: None,
        voice_active: false,
        busy: false,
        interrupts_last_hour: 0,
        suggestions_today: 0,
    }
}

#[test]
fn task_signals_are_pure_and_keep_input_order() {
    let now = at("2026-11-16T18:00:00Z");
    let outcomes = [
        outcome("t1", "refactor", TaskEnd::Failed, now),
        outcome("t2", "build", TaskEnd::Succeeded, now),
    ];
    let first = task_signals(&outcomes, now - Duration::hours(1), now);
    let second = task_signals(&outcomes, now - Duration::hours(1), now);
    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].dedupe_key, "task:t1:task_failed");
    assert_eq!(first[1].dedupe_key, "task:t2:task_succeeded");
}

#[test]
fn window_is_exclusive_at_since_and_inclusive_at_now() {
    let since = at("2026-11-16T17:00:00Z");
    let now = at("2026-11-16T18:00:00Z");
    let outcomes = [
        outcome("old", "too old", TaskEnd::Failed, since),
        outcome(
            "inside",
            "inside",
            TaskEnd::Failed,
            since + Duration::seconds(1),
        ),
        outcome("edge", "edge", TaskEnd::Failed, now),
        outcome(
            "future",
            "future",
            TaskEnd::Failed,
            now + Duration::seconds(1),
        ),
    ];
    let signals = task_signals(&outcomes, since, now);
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[0].dedupe_key, "task:inside:task_failed");
    assert_eq!(signals[1].dedupe_key, "task:edge:task_failed");
}

#[test]
fn fields_match_the_contract_for_every_end_state() {
    let now = at("2026-11-16T18:00:00Z");
    let outcomes = [
        outcome("t1", "refactor", TaskEnd::Succeeded, now),
        outcome("t2", "refactor", TaskEnd::Failed, now),
        outcome("t3", "refactor", TaskEnd::NeedsIntervention, now),
    ];
    let signals = task_signals(&outcomes, now - Duration::hours(1), now);
    let expected = [
        ("task_succeeded", 0.5, "Forge: refactor succeeded"),
        ("task_failed", 0.7, "Forge: refactor failed"),
        ("task_needs_you", 0.85, "Forge: refactor needs you"),
    ];
    for (signal, (kind, urgency, title)) in signals.iter().zip(expected) {
        assert_eq!(signal.source, Source::Projects);
        assert_eq!(signal.kind, kind);
        assert_eq!(signal.urgency, urgency);
        assert_eq!(signal.relevance, 0.8);
        assert_eq!(signal.novelty, 1.0);
        assert_eq!(signal.confidence, 1.0);
        assert_eq!(signal.provenance, Provenance::Observation);
        assert_eq!(signal.deadline, None);
        assert_eq!(signal.link, None);
        assert_eq!(signal.title, title);
        assert_eq!(signal.body, "2 tests failed in sail-flow.");
        assert_eq!(signal.observed_at, now);
    }
}

#[test]
fn long_text_is_bounded() {
    let now = at("2026-11-16T18:00:00Z");
    let mut outcome = outcome(
        "t1",
        &"a very long task title ".repeat(50),
        TaskEnd::Failed,
        now,
    );
    outcome.summary = "a long summary ".repeat(500);
    let signals = task_signals(&[outcome], now - Duration::hours(1), now);
    let signal = &signals[0];
    assert!(signal.title.chars().count() <= MAX_TITLE_CHARS);
    assert!(signal.title.ends_with(" failed"));
    assert!(signal.body.chars().count() <= MAX_BODY_CHARS);
}

#[test]
fn default_settings_route_failure_to_interrupt_and_success_to_suggest() {
    let now = at("2026-11-16T18:00:00Z");
    let outcomes = [
        outcome("t1", "refactor", TaskEnd::Succeeded, now),
        outcome("t2", "refactor", TaskEnd::Failed, now),
        outcome("t3", "refactor", TaskEnd::NeedsIntervention, now),
    ];
    let signals = task_signals(&outcomes, now - Duration::hours(1), now);
    let settings = AttentionSettings::default();
    let ctx = context(now);
    let assessed: Vec<_> = signals
        .iter()
        .map(|signal| score::assess(signal, &ctx, &settings, None, false))
        .collect();

    assert!(
        matches!(assessed[0].delivery, Delivery::Suggest),
        "succeeded: {:?}",
        assessed[0]
    );
    assert!(
        (assessed[0].score - 0.775).abs() < 1e-6,
        "{:?}",
        assessed[0]
    );
    assert!(
        matches!(assessed[1].delivery, Delivery::Interrupt),
        "failed: {:?}",
        assessed[1]
    );
    assert!(
        (assessed[1].score - 0.845).abs() < 1e-6,
        "{:?}",
        assessed[1]
    );
    assert!(
        matches!(assessed[2].delivery, Delivery::Interrupt),
        "needs you: {:?}",
        assessed[2]
    );
    assert!(
        (assessed[2].score - 0.8975).abs() < 1e-6,
        "{:?}",
        assessed[2]
    );
}

#[test]
fn end_states_serialize_with_the_contract_names() {
    assert_eq!(
        serde_json::to_value(TaskEnd::Succeeded).unwrap(),
        serde_json::json!("succeeded")
    );
    assert_eq!(
        serde_json::to_value(TaskEnd::Failed).unwrap(),
        serde_json::json!("failed")
    );
    assert_eq!(
        serde_json::to_value(TaskEnd::NeedsIntervention).unwrap(),
        serde_json::json!("needs_intervention")
    );
}

#[test]
fn title_uses_the_owning_subsystem() {
    let now = at("2026-11-16T18:00:00Z");
    let outcomes = [
        TaskOutcome {
            owner: "flow".into(),
            ..outcome("t1", "refactor", TaskEnd::Failed, now)
        },
        TaskOutcome {
            owner: String::new(),
            ..outcome("t2", "refactor", TaskEnd::Succeeded, now)
        },
        TaskOutcome {
            owner: "   ".into(),
            ..outcome("t3", "refactor", TaskEnd::NeedsIntervention, now)
        },
    ];
    let signals = task_signals(&outcomes, now - Duration::hours(1), now);
    assert_eq!(signals[0].title, "Flow: refactor failed");
    assert_eq!(signals[1].title, "Task: refactor succeeded");
    assert_eq!(signals[2].title, "Task: refactor needs you");
}
