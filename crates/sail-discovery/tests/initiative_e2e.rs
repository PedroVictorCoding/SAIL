//! Cross-slice: Discovery detectors → Pulse attention store → inbox, digest, brief.
//! Red until Pulse's attention store lands (round 15 phase 1).
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use sail_discovery::agenda::{AgendaKind, AgendaStore, NewAgendaItem};
use sail_discovery::detect::{Observation, SchoolChangeKind, SchoolItemKind};
use sail_discovery::initiative::Initiative;
use sail_pulse::attention::store::AttentionStore;
use sail_pulse::attention::{AttentionContext, Delivery, Feedback, ItemState, Provenance};

fn now() -> DateTime<Utc> {
    // A Thursday, 10:00 UTC: not quiet, no focus.
    Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap()
}

fn context(at: DateTime<Utc>) -> AttentionContext {
    AttentionContext {
        now: at,
        quiet_hours: false,
        focus_until: None,
        voice_active: false,
        busy: false,
        interrupts_last_hour: 0,
        suggestions_today: 0,
    }
}

fn engine() -> Initiative {
    Initiative::new(
        Arc::new(AttentionStore::open_in_memory().unwrap()),
        Arc::new(AgendaStore::open_in_memory().unwrap()),
    )
}

fn exam(due_in: Duration) -> Observation {
    Observation::SchoolItem {
        id: "exam-1".into(),
        course: "Física".into(),
        kind: SchoolItemKind::Exam,
        title: "Exame época normal".into(),
        due: now() + due_in,
        weight_percent: Some(60.0),
        confidence: 0.95,
    }
}

#[test]
fn an_imminent_exam_interrupts_once_and_lands_on_the_agenda() {
    let engine = engine();
    let report = engine.tick(&[exam(Duration::hours(2))], context(now()));
    assert_eq!(report.signals, 1);
    assert_eq!(report.interrupt, 1, "{report:?}");
    assert_eq!(report.agenda_updates, 1);
    let agenda = engine.agenda().open_items(10).unwrap();
    assert_eq!(agenda[0].kind, AgendaKind::Commitment);
    assert_eq!(agenda[0].provenance, Provenance::UserCommitment);

    let shown = engine.attention().deliverable(now()).unwrap();
    assert_eq!(shown.len(), 1);
    // Delivered items are not delivered again, and the next tick dedupes.
    assert!(engine.attention().deliverable(now()).unwrap().is_empty());
    let again = engine.tick(
        &[exam(Duration::hours(2))],
        context(now() + Duration::minutes(5)),
    );
    assert_eq!(again.interrupt, 0);
    assert_eq!(again.dropped, 1);
}

#[test]
fn low_value_news_is_digested_not_pushed() {
    let engine = engine();
    let file = Observation::SchoolChange {
        id: "chg-1".into(),
        course: "Cálculo".into(),
        kind: SchoolChangeKind::FileAdded,
        title: "Novo ficheiro: slides semana 9".into(),
        observed_at: now() - Duration::hours(1),
    };
    let report = engine.tick(&[file], context(now()));
    assert_eq!(report.interrupt + report.suggest, 0, "{report:?}");
    assert_eq!(report.digest + report.silent, 1);
}

#[test]
fn quiet_hours_hold_an_observation_but_not_an_imminent_commitment() {
    let engine = engine();
    let mut quiet = context(now());
    quiet.quiet_hours = true;
    let disk = Observation::DiskSpace {
        mount: "/srv/sail-data".into(),
        free_percent: 2.0,
    };
    let report = engine.tick(&[disk, exam(Duration::minutes(90))], quiet);
    assert_eq!(
        report.interrupt, 1,
        "only the exam may interrupt: {report:?}"
    );
}

#[test]
fn later_feedback_snoozes_and_redelivers() {
    let engine = engine();
    engine.tick(&[exam(Duration::hours(2))], context(now()));
    let item = engine.attention().deliverable(now()).unwrap().remove(0);
    let later = now() + Duration::minutes(30);
    let snoozed = engine
        .attention()
        .feedback(&item.id, Feedback::Later(later), now())
        .unwrap();
    assert_eq!(snoozed.state, ItemState::Snoozed);
    assert!(
        engine
            .attention()
            .deliverable(now() + Duration::minutes(10))
            .unwrap()
            .is_empty()
    );
    let back = engine
        .attention()
        .deliverable(later + Duration::minutes(1))
        .unwrap();
    assert_eq!(back.len(), 1);
}

#[test]
fn new_evidence_revisits_an_open_question() {
    let engine = engine();
    engine
        .agenda()
        .upsert(
            NewAgendaItem {
                key: "q:fisica-peso".into(),
                kind: AgendaKind::OpenQuestion,
                title: "How much does the Física project count?".into(),
                detail: String::new(),
                provenance: Provenance::Suggestion,
                match_terms: vec!["física".into(), "projeto".into()],
                due: None,
            },
            now(),
        )
        .unwrap();
    let change = Observation::SchoolChange {
        id: "chg-2".into(),
        course: "Física".into(),
        kind: SchoolChangeKind::AnnouncementPosted,
        title: "Ponderação do projeto: 30%".into(),
        observed_at: now(),
    };
    let report = engine.tick(&[change], context(now()));
    assert_eq!(report.revisits, 1);
    assert_eq!(report.signals, 2);
    let inbox = engine.attention().inbox(10).unwrap();
    assert!(inbox.iter().any(|i| i.signal.kind == "revisit"));
}

#[test]
fn the_brief_is_short_tagged_and_empty_when_idle() {
    let engine = engine();
    assert!(engine.conversation_brief(now()).is_empty());
    engine.tick(&[exam(Duration::days(2))], context(now()));
    let brief = engine.conversation_brief(now());
    assert!(brief.contains("due soon"), "{brief}");
    assert!(brief.chars().count() <= sail_discovery::initiative::MAX_BRIEF_CHARS);
}

#[test]
fn every_assessment_is_explained() {
    let engine = engine();
    engine.tick(&[exam(Duration::days(10))], context(now()));
    let item = engine.attention().inbox(1).unwrap().remove(0);
    assert!(!item.assessment.reasons.is_empty());
    assert!(!matches!(item.assessment.delivery, Delivery::Drop(_)));
}
