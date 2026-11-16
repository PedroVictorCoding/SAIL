//! Hands-free addressing, the follow-up window and the arrival greeting.
use chrono::{DateTime, Duration, Utc};
use sail_dialogue::address::{Addressed, FollowUpWindow, addressed};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn rest(transcript: &str) -> Option<String> {
    match addressed(transcript) {
        Addressed::Yes { rest } => Some(rest),
        Addressed::No => None,
    }
}

#[test]
fn addressed_keeps_the_original_case_of_the_request() {
    assert_eq!(
        rest("SAIL, what's next today?").as_deref(),
        Some("what's next today?")
    );
    assert_eq!(rest("sail open the map").as_deref(), Some("open the map"));
    assert_eq!(rest("Um, SAIL, what next?").as_deref(), Some("what next?"));
    assert_eq!(
        rest("Hey Sail, open the map").as_deref(),
        Some("open the map")
    );
    assert_eq!(rest("Hi SAIL").as_deref(), Some(""));
    assert_eq!(rest("Sail.").as_deref(), Some(""));
    assert_eq!(rest("SAIL").as_deref(), Some(""));
}

#[test]
fn addressed_is_pure() {
    let transcript = "um, hey SAIL, what's next?";
    assert_eq!(addressed(transcript), addressed(transcript));
}

#[test]
fn name_must_start_within_the_first_three_words() {
    assert_eq!(rest("so hey sail what next").as_deref(), Some("what next"));
    assert_eq!(rest("one two sail now").as_deref(), Some("now"));
    assert_eq!(rest("one two three hey sail"), None);
    assert_eq!(rest("one two three sail"), None);
}

#[test]
fn whole_words_only() {
    assert_eq!(rest("sailing is fun"), None);
    assert_eq!(rest("sailboat race"), None);
    assert_eq!(rest("sails"), None);
    assert_eq!(rest("sailor"), None);
}

#[test]
fn other_wake_names_and_accents() {
    assert_eq!(rest("Olá SAIL, bom dia").as_deref(), Some("bom dia"));
    assert_eq!(rest("ola sail bom dia").as_deref(), Some("bom dia"));
    assert_eq!(rest("ó sail, diz lá").as_deref(), Some("diz lá"));
    assert_eq!(rest("oi sail tudo bem").as_deref(), Some("tudo bem"));
    assert_eq!(rest("okay sail what next").as_deref(), Some("what next"));
    assert_eq!(rest("ok sail what next").as_deref(), Some("what next"));
}

#[test]
fn leading_punctuation_and_spaces_are_trimmed_but_not_the_tail() {
    assert_eq!(rest("Sail - what's next").as_deref(), Some("what's next"));
    assert_eq!(
        rest("Sail, ... what's next ").as_deref(),
        Some("what's next ")
    );
    assert_eq!(
        rest("Sail — what's next").as_deref(),
        Some("— what's next"),
        "only the listed leading characters are trimmed"
    );
}

#[test]
fn hostile_and_empty_input_never_panics() {
    assert_eq!(rest(""), None);
    assert_eq!(rest("   "), None);
    assert_eq!(rest("!!! ???"), None);
    assert_eq!(rest(&"a".repeat(2_000)), None);
    let long = format!("SAIL {}", "x".repeat(2_000));
    assert_eq!(
        rest(&long),
        None,
        "over 2000 chars is rejected, not scanned"
    );
    // Exactly at the cap is still scanned.
    let at_cap = format!("SAIL {}", "x".repeat(1_995));
    assert!(rest(&at_cap).is_some());
}

#[test]
fn follow_up_window_opens_for_eight_seconds() {
    let now = at("2026-11-16T18:00:00Z");
    let mut window = FollowUpWindow::new();
    assert!(!window.accepts(now));

    window.open(now);
    assert!(window.accepts(now));
    assert_eq!(window.accepts(now), window.accepts(now), "pure");
    assert!(window.accepts(now + Duration::seconds(8)));
    assert!(!window.accepts(now + Duration::seconds(9)));

    // Re-opening moves the end.
    window.open(now + Duration::seconds(5));
    assert!(window.accepts(now + Duration::seconds(13)));
    assert!(!window.accepts(now + Duration::seconds(14)));

    window.close();
    assert!(!window.accepts(now + Duration::seconds(1)));
}

use sail_dialogue::briefing::{BriefingEvent, ProjectContinuity, ReturnBriefing};
use sail_dialogue::greeting::{Lang, MAX_SPOKEN_CHARS, compose};
use sail_pulse::attention::{
    Assessment, AttentionItem, Delivery, ItemState, Provenance, Signal, Source,
};

fn briefing() -> ReturnBriefing {
    ReturnBriefing {
        id: "briefing-1".into(),
        generated_at: at("2026-11-16T17:30:00Z"),
        local_time_str: "17:30".into(),
        greeting: "Hello".into(),
        headline: "A headline".into(),
        away_duration_minutes: Some(60),
        staged_music: None,
        learning_plan: None,
        events_to_watch: Vec::new(),
        news_radar: Vec::new(),
        project_continuity: None,
        system_sentinel: None,
        dismissed: false,
    }
}

fn held(
    id: &str,
    title: &str,
    score: f32,
    delivery: Delivery,
    created_at: DateTime<Utc>,
) -> AttentionItem {
    AttentionItem {
        id: id.into(),
        signal: Signal {
            source: Source::Projects,
            kind: "task_failed".into(),
            dedupe_key: format!("task:{id}"),
            title: title.into(),
            body: String::new(),
            urgency: 0.7,
            relevance: 0.8,
            novelty: 1.0,
            confidence: 1.0,
            deadline: None,
            provenance: Provenance::Observation,
            link: None,
            observed_at: created_at,
        },
        assessment: Assessment {
            score,
            interruption_cost: 0.2,
            delivery,
            reasons: Vec::new(),
        },
        state: ItemState::Pending,
        snoozed_until: None,
        created_at,
        updated_at: created_at,
        last_feedback: None,
    }
}

fn event(id: &str, title: &str, due_at: DateTime<Utc>) -> BriefingEvent {
    BriefingEvent {
        id: id.into(),
        title: title.into(),
        due_at,
        due_relative: "ignored on purpose".into(),
        urgency: "high".into(),
        source: "pulse".into(),
        action_type: "reminder".into(),
    }
}

#[test]
fn opening_follows_the_local_hour_in_both_languages() {
    let now = at("2026-11-16T18:00:00Z");
    let empty = briefing();
    let cases = [
        (5, Lang::En, "Good morning."),
        (11, Lang::En, "Good morning."),
        (12, Lang::En, "Welcome back."),
        (19, Lang::En, "Welcome back."),
        (20, Lang::En, "Good evening."),
        (4, Lang::En, "Good evening."),
        (5, Lang::PtPt, "Bom dia."),
        (12, Lang::PtPt, "Bem-vindo de volta."),
        (21, Lang::PtPt, "Boa noite."),
    ];
    for (hour, lang, opening) in cases {
        let greeting = compose(&empty, &[], 3_600, now, hour, lang);
        assert_eq!(greeting.spoken, opening, "hour {hour} {lang:?}");
        assert!(greeting.item_ids.is_empty());
    }
}

#[test]
fn nothing_to_say_is_the_opening_alone() {
    let now = at("2026-11-16T18:00:00Z");
    let greeting = compose(&briefing(), &[], 3_600, now, 18, Lang::En);
    assert_eq!(greeting.spoken, "Welcome back.");
    assert_eq!(
        compose(&briefing(), &[], 3_600, now, 18, Lang::En),
        greeting
    );
}

#[test]
fn picks_the_highest_score_held_item_and_newest_on_a_tie() {
    let now = at("2026-11-16T18:00:00Z");
    let older = held(
        "a1",
        "Forge: refactor failed",
        0.9,
        Delivery::Suggest,
        now - Duration::seconds(60),
    );
    let newer = held(
        "b2",
        "Quiz 3",
        0.9,
        Delivery::Suggest,
        now - Duration::seconds(10),
    );
    let lower = held("c3", "Disk is nearly full", 0.5, Delivery::Interrupt, now);
    let greeting = compose(
        &briefing(),
        &[lower, older, newer],
        3_600,
        now,
        18,
        Lang::En,
    );
    assert_eq!(greeting.spoken, "Welcome back. Quiz 3.");
    assert_eq!(greeting.item_ids, vec!["b2".to_owned()]);
}

#[test]
fn only_interrupt_and_suggest_items_are_greeted() {
    let now = at("2026-11-16T18:00:00Z");
    for delivery in [
        Delivery::Digest,
        Delivery::Silent,
        Delivery::Drop(sail_pulse::attention::DropReason::Duplicate),
    ] {
        let item = held("a1", "Hidden", 1.0, delivery, now);
        let greeting = compose(&briefing(), &[item], 3_600, now, 18, Lang::En);
        assert_eq!(greeting.spoken, "Welcome back.");
        assert!(greeting.item_ids.is_empty());
    }
}

#[test]
fn mentions_the_earliest_event_due_within_twelve_hours() {
    let now = at("2026-11-16T18:00:00Z");
    let mut plan = briefing();
    plan.events_to_watch = vec![
        event("late", "Next week's exam", now + Duration::hours(13)),
        event("past", "Already happened", now - Duration::minutes(1)),
        event("soon", "Quiz 3", now + Duration::hours(2)),
        event("sooner", "Lab report", now + Duration::minutes(30)),
    ];
    let greeting = compose(&plan, &[], 3_600, now, 18, Lang::En);
    assert_eq!(
        greeting.spoken,
        "Welcome back. Lab report is due in 30 minutes."
    );

    plan.events_to_watch = vec![event("edge", "Twelve hours", now + Duration::hours(12))];
    assert_eq!(
        compose(&plan, &[], 3_600, now, 18, Lang::En).spoken,
        "Welcome back. Twelve hours is due in 12 hours."
    );
    plan.events_to_watch = vec![event(
        "edge",
        "Just over",
        now + Duration::hours(12) + Duration::seconds(1),
    )];
    assert_eq!(
        compose(&plan, &[], 3_600, now, 18, Lang::En).spoken,
        "Welcome back."
    );
    plan.events_to_watch = vec![event("now", "Due now", now)];
    assert_eq!(
        compose(&plan, &[], 3_600, now, 18, Lang::En).spoken,
        "Welcome back. Due now is due in 1 minute."
    );
}

#[test]
fn at_most_two_clauses_held_then_event_then_continuity() {
    let now = at("2026-11-16T18:00:00Z");
    let mut plan = briefing();
    plan.events_to_watch = vec![event("e1", "Quiz 3", now + Duration::hours(2))];
    plan.project_continuity = Some(ProjectContinuity {
        active_workspace: "SAIL".into(),
        forge_summary: None,
        last_edited_file: None,
        suggested_action: "Continue the refactor".into(),
    });
    let item = held("a1", "Forge: refactor failed", 0.9, Delivery::Suggest, now);
    let greeting = compose(&plan, &[item], 3_600, now, 18, Lang::En);
    assert_eq!(
        greeting.spoken,
        "Welcome back. Forge: refactor failed, and Quiz 3 is due in 2 hours."
    );
    assert_eq!(greeting.item_ids, vec!["a1".to_owned()]);

    // Without a held item or event, the next step is used.
    let only_plan = compose(&plan, &[], 3_600, now, 18, Lang::En);
    assert_eq!(
        only_plan.spoken,
        "Welcome back. Quiz 3 is due in 2 hours, and Continue the refactor."
    );
    plan.events_to_watch.clear();
    assert_eq!(
        compose(&plan, &[], 3_600, now, 18, Lang::En).spoken,
        "Welcome back. Continue the refactor."
    );
}

#[test]
fn clauses_are_trimmed_at_a_word_boundary() {
    let now = at("2026-11-16T18:00:00Z");
    let long_word = "a".repeat(75);
    let title = format!("{long_word} EXTRAORDINARILY");
    let item = held("a1", &title, 0.9, Delivery::Suggest, now);
    let greeting = compose(&briefing(), &[item], 3_600, now, 18, Lang::En);
    assert!(greeting.spoken.starts_with("Welcome back. "));
    assert!(greeting.spoken.contains(&long_word));
    assert!(!greeting.spoken.contains("EXTRA"), "{}", greeting.spoken);
    assert!(greeting.spoken.chars().count() <= MAX_SPOKEN_CHARS);
}

#[test]
fn speaks_portuguese_and_never_exceeds_the_cap() {
    let now = at("2026-11-16T18:00:00Z");
    let mut plan = briefing();
    plan.events_to_watch = vec![event("e1", "Quiz 3", now + Duration::hours(2))];
    let item = held("a1", "Forge: refactor failed", 0.9, Delivery::Suggest, now);
    assert_eq!(
        compose(&plan, &[item], 3_600, now, 18, Lang::PtPt).spoken,
        "Bem-vindo de volta. Forge: refactor failed, e Quiz 3 é daqui a 2 horas."
    );

    let mut plan = briefing();
    plan.events_to_watch = vec![event(
        "e1",
        &"very long event title ".repeat(10),
        now + Duration::hours(2),
    )];
    plan.project_continuity = Some(ProjectContinuity {
        active_workspace: "SAIL".into(),
        forge_summary: None,
        last_edited_file: None,
        suggested_action: "and another very long next step ".repeat(10),
    });
    let item = held(
        "a1",
        &"forge title that keeps going ".repeat(10),
        0.9,
        Delivery::Suggest,
        now,
    );
    let greeting = compose(&plan, &[item], 3_600, now, 18, Lang::En);
    assert!(greeting.spoken.chars().count() <= MAX_SPOKEN_CHARS);
}
