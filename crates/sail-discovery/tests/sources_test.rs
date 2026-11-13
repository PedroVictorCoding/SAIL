//! Beyond school: Capital and Forge detectors, and Moodle calibrated so it doesn't
//! dominate. Each case is checked through the real attention scoring.
use chrono::{DateTime, Duration, TimeZone, Utc};
use sail_discovery::detect::{
    ForgeJobState, Level, Observation, SchoolChangeKind, SchoolItemKind, detect,
};
use sail_pulse::attention::store::AttentionStore;
use sail_pulse::attention::{AttentionContext, Delivery, Source};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap()
}

fn delivery(observation: &Observation) -> Option<Delivery> {
    let signal = detect(observation, now()).into_iter().next()?;
    let store = AttentionStore::open_in_memory().unwrap();
    let item = store
        .ingest(
            signal,
            AttentionContext {
                now: now(),
                quiet_hours: false,
                focus_until: None,
                voice_active: false,
                busy: false,
                interrupts_last_hour: 0,
                suggestions_today: 0,
            },
        )
        .unwrap();
    Some(item.assessment.delivery)
}

fn capital(severity: Level) -> Observation {
    Observation::CapitalSuggestion {
        id: "abc".into(),
        severity,
        title: "Free funds are 37% of the account; your limit is 15%".into(),
        why: "Cash above the policy limit.".into(),
    }
}

#[test]
fn capital_high_is_raised_medium_waits_for_the_digest_low_is_ignored() {
    assert_eq!(delivery(&capital(Level::High)), Some(Delivery::Suggest));
    assert_eq!(delivery(&capital(Level::Medium)), Some(Delivery::Digest));
    assert_eq!(delivery(&capital(Level::Low)), None);
    let signal = &detect(&capital(Level::High), now())[0];
    assert_eq!(signal.source, Source::Capital);
    assert_eq!(signal.link.as_deref(), Some("/capital"));
}

#[test]
fn forge_jobs_are_news_once_per_state() {
    let job = |state| Observation::ForgeJob {
        job_id: "j1".into(),
        subject: "owner/sail#12".into(),
        state,
    };
    assert_eq!(
        delivery(&job(ForgeJobState::AwaitingUser)),
        Some(Delivery::Suggest)
    );
    assert_eq!(
        delivery(&job(ForgeJobState::ReviewPending)),
        Some(Delivery::Suggest)
    );
    assert_eq!(
        delivery(&job(ForgeJobState::Completed)),
        Some(Delivery::Digest)
    );
    let a = &detect(&job(ForgeJobState::Blocked), now())[0];
    let b = &detect(&job(ForgeJobState::ReviewPending), now())[0];
    assert_eq!(a.source, Source::Projects);
    assert_ne!(a.dedupe_key, b.dedupe_key);
}

#[test]
fn school_is_quieter_two_weeks_out_and_course_churn_goes_to_the_digest() {
    let item = |days, weight| Observation::SchoolItem {
        id: "i".into(),
        course: "Cálculo II".into(),
        kind: SchoolItemKind::Deadline,
        title: "Entrega".into(),
        due: now() + Duration::days(days),
        weight_percent: weight,
        confidence: 0.9,
    };
    assert_eq!(delivery(&item(10, None)), Some(Delivery::Digest));
    assert_eq!(delivery(&item(2, None)), Some(Delivery::Suggest));

    let change = |kind| Observation::SchoolChange {
        id: "c".into(),
        course: "Física".into(),
        kind,
        title: "x".into(),
        observed_at: now(),
    };
    for kind in [
        SchoolChangeKind::AnnouncementPosted,
        SchoolChangeKind::AssignmentAdded,
        SchoolChangeKind::EventAdded,
        SchoolChangeKind::FileAdded,
    ] {
        assert_eq!(delivery(&change(kind)), Some(Delivery::Digest), "{kind:?}");
    }
    // Worth raising, but no longer an interruption on its own.
    assert_eq!(
        delivery(&change(SchoolChangeKind::DeadlineMoved)),
        Some(Delivery::Suggest)
    );
    assert_eq!(
        delivery(&change(SchoolChangeKind::GradeReleased)),
        Some(Delivery::Suggest)
    );
}
