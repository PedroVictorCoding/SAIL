use chrono::{Duration, TimeZone, Utc};
use sail_discovery::agenda::{AgendaKind, AgendaStatus, AgendaStore, NewAgendaItem};
use sail_discovery::detect::{Observation, SchoolChangeKind, SchoolItemKind, detect};
use sail_pulse::attention::{Provenance, Source};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap()
}

fn item(due_in: Duration) -> Observation {
    Observation::SchoolItem {
        id: "abc".into(),
        course: "Cálculo II".into(),
        kind: SchoolItemKind::Test,
        title: "1.º Teste".into(),
        due: now() + due_in,
        weight_percent: Some(40.0),
        confidence: 0.9,
    }
}

#[test]
fn school_item_stages_have_distinct_keys_and_rising_urgency() {
    let far = detect(&item(Duration::days(10)), now());
    let near = detect(&item(Duration::days(2)), now());
    let day = detect(&item(Duration::hours(5)), now());
    assert_eq!(far.len(), 1);
    assert!(far[0].urgency < near[0].urgency && near[0].urgency < day[0].urgency);
    assert_ne!(far[0].dedupe_key, near[0].dedupe_key);
    assert_ne!(near[0].dedupe_key, day[0].dedupe_key);
    assert_eq!(day[0].provenance, Provenance::UserCommitment);
    assert_eq!(day[0].source, Source::Moodle);
    assert!(day[0].body.contains("40%"));
    // Same facts → same signal.
    assert_eq!(detect(&item(Duration::hours(5)), now()), day);
}

#[test]
fn school_items_outside_the_horizon_or_past_are_ignored() {
    assert!(detect(&item(Duration::days(30)), now()).is_empty());
    assert!(detect(&item(-Duration::hours(1)), now()).is_empty());
}

#[test]
fn old_changes_are_not_news() {
    let change = |age| Observation::SchoolChange {
        id: "c1".into(),
        course: "Física".into(),
        kind: SchoolChangeKind::GradeReleased,
        title: "Nota do teste".into(),
        observed_at: now() - age,
    };
    assert_eq!(detect(&change(Duration::hours(2)), now()).len(), 1);
    assert!(detect(&change(Duration::days(8)), now()).is_empty());
}

#[test]
fn disk_alerts_only_when_low() {
    let disk = |free| Observation::DiskSpace {
        mount: "/srv/sail-data".into(),
        free_percent: free,
    };
    assert!(detect(&disk(25.0), now()).is_empty());
    let low = detect(&disk(8.0), now());
    let critical = detect(&disk(2.0), now());
    assert!(critical[0].urgency > low[0].urgency);
    assert_ne!(low[0].dedupe_key, critical[0].dedupe_key);
    assert!(detect(&disk(f32::NAN), now()).is_empty());
}

#[test]
fn agenda_upsert_is_keyed_and_keeps_provenance() {
    let store = AgendaStore::open_in_memory().unwrap();
    let new = |title: &str| NewAgendaItem {
        key: "q:rust-async".into(),
        kind: AgendaKind::OpenQuestion,
        title: title.into(),
        detail: String::new(),
        provenance: Provenance::Hypothesis,
        match_terms: vec!["Tokio".into(), "cancellation".into(), "io".into()],
        due: None,
    };
    let a = store
        .upsert(new("Is tokio cancellation safe here?"), now())
        .unwrap();
    let b = store
        .upsert(new("Is Tokio cancellation safe?"), now())
        .unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(b.title, "Is Tokio cancellation safe?");
    assert_eq!(b.provenance, Provenance::Hypothesis);
    // Short terms are dropped, others lowercased.
    assert_eq!(b.match_terms, vec!["tokio", "cancellation"]);
    assert_eq!(store.open_items(10).unwrap().len(), 1);

    store
        .set_status(&a.id, AgendaStatus::Resolved, now())
        .unwrap();
    // Re-detection does not reopen a resolved item.
    let c = store.upsert(new("again"), now()).unwrap();
    assert_eq!(c.status, AgendaStatus::Resolved);
    assert!(store.open_items(10).unwrap().is_empty());
}

#[test]
fn evidence_matches_open_questions_only() {
    let store = AgendaStore::open_in_memory().unwrap();
    let q = store
        .upsert(
            NewAgendaItem {
                key: "q:1".into(),
                kind: AgendaKind::OpenQuestion,
                title: "Which grading weight does the project have?".into(),
                detail: String::new(),
                provenance: Provenance::Suggestion,
                match_terms: vec!["projeto".into(), "ponderação".into()],
                due: None,
            },
            now(),
        )
        .unwrap();
    assert!(
        store
            .match_evidence("Novo ficheiro: horário", now())
            .unwrap()
            .is_empty()
    );
    // Two of two terms needed.
    assert!(
        store
            .match_evidence("Projeto final", now())
            .unwrap()
            .is_empty()
    );
    let hits = store
        .match_evidence("Ponderação do PROJETO atualizada", now())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, q.id);
    assert!(hits[0].last_evidence.is_some());
}

#[test]
fn agenda_survives_reopen() {
    let dir = std::env::temp_dir().join(format!("sail-agenda-{}", uuid::Uuid::new_v4()));
    let path = dir.join("agenda.db");
    {
        let store = AgendaStore::open(&path).unwrap();
        store
            .upsert(
                NewAgendaItem {
                    key: "c:1".into(),
                    kind: AgendaKind::Commitment,
                    title: "Entrega".into(),
                    detail: String::new(),
                    provenance: Provenance::UserCommitment,
                    match_terms: vec![],
                    due: Some(now() + Duration::days(2)),
                },
                now(),
            )
            .unwrap();
    }
    let store = AgendaStore::open(&path).unwrap();
    let items = store.open_items(10).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].due, Some(now() + Duration::days(2)));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rejects_empty_items() {
    let store = AgendaStore::open_in_memory().unwrap();
    let err = store.upsert(
        NewAgendaItem {
            key: " ".into(),
            kind: AgendaKind::Topic,
            title: "x".into(),
            detail: String::new(),
            provenance: Provenance::Suggestion,
            match_terms: vec![],
            due: None,
        },
        now(),
    );
    assert!(err.is_err());
}
