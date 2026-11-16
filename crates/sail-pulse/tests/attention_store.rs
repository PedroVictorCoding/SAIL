use chrono::{DateTime, Duration, TimeZone, Timelike, Utc};
use sail_pulse::attention::{
    AttentionContext, Delivery, DropReason, Feedback, ItemState, MAX_DIGEST_ITEMS, Provenance,
    Signal, Source, store::AttentionStore,
};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap()
}

fn ctx(now: DateTime<Utc>) -> AttentionContext {
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

#[allow(clippy::too_many_arguments)]
fn make_signal(
    source: Source,
    kind: &str,
    dedupe_key: &str,
    urgency: f32,
    relevance: f32,
    novelty: f32,
    confidence: f32,
    deadline: Option<DateTime<Utc>>,
    provenance: Provenance,
    now: DateTime<Utc>,
) -> Signal {
    Signal {
        source,
        kind: kind.into(),
        dedupe_key: dedupe_key.into(),
        title: format!("{kind} ({dedupe_key})"),
        body: "details".into(),
        urgency,
        relevance,
        novelty,
        confidence,
        deadline,
        provenance,
        link: None,
        observed_at: now,
    }
}

#[test]
fn interrupt_budget_per_hour_is_enforced() {
    let store = AttentionStore::open_in_memory().unwrap();
    let now = t0();

    for i in 0..3 {
        let sig = make_signal(
            Source::Reminders,
            "due",
            &format!("rem:{i}"),
            1.0,
            1.0,
            1.0,
            1.0,
            Some(now + Duration::minutes(30)),
            Provenance::UserCommitment,
            now,
        );
        let item = store.ingest(sig, ctx(now + Duration::minutes(i))).unwrap();
        if i < 2 {
            assert_eq!(item.assessment.delivery, Delivery::Interrupt);
        } else {
            assert_ne!(
                item.assessment.delivery,
                Delivery::Interrupt,
                "3rd interrupt in 1h must be capped"
            );
        }
    }

    // After 1 hour + 1 minute, budget resets.
    let later = now + Duration::minutes(65);
    let sig = make_signal(
        Source::Reminders,
        "due",
        "rem:3",
        1.0,
        1.0,
        1.0,
        1.0,
        Some(later + Duration::minutes(30)),
        Provenance::UserCommitment,
        later,
    );
    let item = store.ingest(sig, ctx(later)).unwrap();
    assert_eq!(item.assessment.delivery, Delivery::Interrupt);
}

#[test]
fn duplicate_inside_cooldown_is_dropped() {
    let store = AttentionStore::open_in_memory().unwrap();
    let now = t0();
    let sig = make_signal(
        Source::Moodle,
        "grade",
        "moodle:grade:42",
        0.7,
        0.8,
        0.8,
        0.9,
        None,
        Provenance::Observation,
        now,
    );
    let first = store.ingest(sig.clone(), ctx(now)).unwrap();
    assert!(!matches!(first.assessment.delivery, Delivery::Drop(_)));

    let dup = store
        .ingest(sig.clone(), ctx(now + Duration::hours(2)))
        .unwrap();
    assert_eq!(
        dup.assessment.delivery,
        Delivery::Drop(DropReason::Duplicate)
    );
    assert_eq!(dup.state, ItemState::Dismissed);

    // After 12h cooldown expires, it is accepted again.
    let after = store.ingest(sig, ctx(now + Duration::hours(13))).unwrap();
    assert!(!matches!(after.assessment.delivery, Delivery::Drop(_)));
}

#[test]
fn mute_drops_and_unmute_restores() {
    let store = AttentionStore::open_in_memory().unwrap();
    let now = t0();
    let sig1 = make_signal(
        Source::Curiosity,
        "paper",
        "paper:1",
        0.6,
        0.7,
        0.8,
        0.8,
        None,
        Provenance::Hypothesis,
        now,
    );
    let item1 = store.ingest(sig1, ctx(now)).unwrap();
    store.feedback(&item1.id, Feedback::Mute, now).unwrap();

    let sig2 = make_signal(
        Source::Curiosity,
        "paper",
        "paper:2",
        0.6,
        0.7,
        0.8,
        0.8,
        None,
        Provenance::Hypothesis,
        now + Duration::minutes(5),
    );
    let item2 = store.ingest(sig2, ctx(now + Duration::minutes(5))).unwrap();
    assert_eq!(item2.assessment.delivery, Delivery::Drop(DropReason::Muted));

    store.unmute(Source::Curiosity, "paper").unwrap();
    let sig3 = make_signal(
        Source::Curiosity,
        "paper",
        "paper:3",
        0.6,
        0.7,
        0.8,
        0.8,
        None,
        Provenance::Hypothesis,
        now + Duration::minutes(10),
    );
    let item3 = store
        .ingest(sig3, ctx(now + Duration::minutes(10)))
        .unwrap();
    assert!(!matches!(item3.assessment.delivery, Delivery::Drop(_)));
}

#[test]
fn later_snoozes_and_redelivers_and_feedback_transitions_work() {
    let store = AttentionStore::open_in_memory().unwrap();
    let now = t0();
    let sig = make_signal(
        Source::Reminders,
        "task",
        "task:1",
        0.95,
        0.95,
        0.9,
        0.95,
        Some(now + Duration::hours(1)),
        Provenance::UserCommitment,
        now,
    );
    let item = store.ingest(sig, ctx(now)).unwrap();
    assert_eq!(item.assessment.delivery, Delivery::Interrupt);

    let shown = store.deliverable(now).unwrap();
    assert_eq!(shown.len(), 1);

    let wake = now + Duration::minutes(30);
    let snoozed = store
        .feedback(&item.id, Feedback::Later(wake), now)
        .unwrap();
    assert_eq!(snoozed.state, ItemState::Snoozed);
    assert!(
        store
            .deliverable(now + Duration::minutes(15))
            .unwrap()
            .is_empty()
    );
    let back = store.deliverable(wake).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].state, ItemState::Delivered);

    let inv = store
        .feedback(&item.id, Feedback::Investigate, wake)
        .unwrap();
    assert_eq!(inv.state, ItemState::Investigating);

    let not_now = store.feedback(&item.id, Feedback::NotNow, wake).unwrap();
    assert_eq!(not_now.state, ItemState::Pending);
    assert_eq!(not_now.assessment.delivery, Delivery::Digest);

    let useful = store.feedback(&item.id, Feedback::Useful, wake).unwrap();
    assert_eq!(useful.state, ItemState::Done);
}

#[test]
fn digest_batches_cap_at_12_and_do_not_repeat() {
    let store = AttentionStore::open_in_memory().unwrap();
    let now = t0();

    for i in 0..15 {
        let sig = make_signal(
            Source::Moodle,
            "file",
            &format!("file:{i}"),
            0.2,
            0.4,
            0.5,
            0.7,
            None,
            Provenance::Observation,
            now + Duration::seconds(i),
        );
        let item = store.ingest(sig, ctx(now + Duration::seconds(i))).unwrap();
        assert_eq!(item.assessment.delivery, Delivery::Digest);
    }

    let batch1 = store
        .release_digest(now + Duration::minutes(1))
        .unwrap()
        .unwrap();
    assert_eq!(batch1.items.len(), MAX_DIGEST_ITEMS);

    let batch2 = store
        .release_digest(now + Duration::minutes(2))
        .unwrap()
        .unwrap();
    assert_eq!(batch2.items.len(), 3);

    assert!(
        store
            .release_digest(now + Duration::minutes(3))
            .unwrap()
            .is_none()
    );
}

#[test]
fn persistence_across_reopen_stats_and_prune() {
    let dir = std::env::temp_dir().join(format!("sail-pulse-test-{}", uuid::Uuid::new_v4()));
    let db_path = dir.join("attention.db");
    let now = t0();

    let item_id = {
        let store = AttentionStore::open(&db_path).unwrap();
        let mut settings = store.settings().unwrap();
        settings.max_interrupts_per_hour = 3;
        store.save_settings(&settings).unwrap();

        let sig = make_signal(
            Source::Capital,
            "alert",
            "cap:1",
            0.8,
            0.8,
            0.8,
            0.8,
            None,
            Provenance::Observation,
            now,
        );
        let item = store.ingest(sig, ctx(now)).unwrap();
        store.feedback(&item.id, Feedback::Dismiss, now).unwrap();
        item.id
    };

    {
        let store = AttentionStore::open(&db_path).unwrap();
        assert_eq!(store.settings().unwrap().max_interrupts_per_hour, 3);
        let fetched = store.get(&item_id).unwrap();
        assert_eq!(fetched.state, ItemState::Dismissed);

        let stats = store.stats(now).unwrap();
        assert!(!stats.by_delivery.is_empty());
        assert!(!stats.feedback.is_empty());

        let pruned = store.prune(7, now + Duration::days(10)).unwrap();
        assert_eq!(pruned, 1);
        assert!(store.get(&item_id).is_err());
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn simulated_week_acceptance_200_signals() {
    let store = AttentionStore::open_in_memory().unwrap();
    let start = Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 0).unwrap();

    let mut total_signals = 0usize;
    let mut interrupts = 0usize;
    let mut digest_batches = 0usize;
    let mut digest_items = 0usize;
    let mut missed_deadlines = 0usize;

    // Track critical commitments (due within 2h) to ensure none are missed.
    let mut critical_ids = Vec::new();

    // 7 days * 24 hours = 168 hours; emit ~200 signals across the week.
    for hour in 0..168 {
        let now = start + Duration::hours(hour);
        let hour_of_day = now.hour();
        let quiet = !(7..23).contains(&hour_of_day);
        let focus = if (9..12).contains(&hour_of_day) || (14..16).contains(&hour_of_day) {
            Some(now + Duration::minutes(30))
        } else {
            None
        };
        let c = AttentionContext {
            now,
            quiet_hours: quiet,
            focus_until: focus,
            voice_active: hour % 19 == 0,
            busy: hour % 7 == 0,
            interrupts_last_hour: 0,
            suggestions_today: 0,
        };

        // Routine hourly observation (168 signals).
        let routine = make_signal(
            Source::Moodle,
            "file",
            &format!("moodle:file:{hour}"),
            0.25,
            0.45,
            0.55,
            0.85,
            None,
            Provenance::Observation,
            now,
        );
        store.ingest(routine, c.clone()).unwrap();
        total_signals += 1;

        // Occasional curiosity/research suggestions (~24 signals).
        if hour % 7 == 0 {
            let idea = make_signal(
                Source::Curiosity,
                "paper",
                &format!("paper:{hour}"),
                0.35,
                0.7,
                0.85,
                0.8,
                None,
                Provenance::Hypothesis,
                now,
            );
            store.ingest(idea, c.clone()).unwrap();
            total_signals += 1;
        }

        // Imminent user commitments (~8 signals, some in quiet/focus hours).
        if hour % 21 == 2 {
            let commit = make_signal(
                Source::Reminders,
                "exam",
                &format!("exam:{hour}"),
                0.96,
                0.95,
                0.9,
                0.98,
                Some(now + Duration::minutes(75)),
                Provenance::UserCommitment,
                now,
            );
            let item = store.ingest(commit, c.clone()).unwrap();
            critical_ids.push(item.id);
            total_signals += 1;
        }

        let delivered = store.deliverable(now).unwrap();
        interrupts += delivered.len();

        // Release digests at 08:00 and 18:00 UTC.
        if matches!(hour_of_day, 8 | 18) {
            let batch = store.release_digest(now).unwrap();
            if let Some(batch) = batch {
                digest_batches += 1;
                digest_items += batch.items.len();
            }
        }
    }

    for id in &critical_ids {
        let item = store.get(id).unwrap();
        if item.state != ItemState::Delivered {
            missed_deadlines += 1;
        }
    }

    println!(
        "simulated week: signals={total_signals}, interrupts={interrupts}, \
         digest_batches={digest_batches}, digest_items={digest_items}, \
         missed_deadlines={missed_deadlines}"
    );
    assert!(total_signals >= 195);
    assert_eq!(missed_deadlines, 0, "no imminent commitment may be missed");
    assert!(
        interrupts <= 20,
        "must not flood notifications: {interrupts}"
    );
    assert!(digest_batches > 0 && digest_items > 0);
}

#[test]
fn pending_interrupts_peeks_without_marking_and_matches_deliverable() {
    let store = AttentionStore::open_in_memory().unwrap();
    let now = t0();
    let sig = make_signal(
        Source::Reminders,
        "task",
        "task:peek",
        0.95,
        0.95,
        0.9,
        0.95,
        Some(now + Duration::hours(1)),
        Provenance::UserCommitment,
        now,
    );
    let item = store.ingest(sig, ctx(now)).unwrap();
    assert_eq!(item.assessment.delivery, Delivery::Interrupt);

    let first = store.pending_interrupts(now).unwrap();
    let second = store.pending_interrupts(now).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].state, ItemState::Pending);

    store.mark_delivered(&item.id, now).unwrap();
    assert!(store.pending_interrupts(now).unwrap().is_empty());
    assert!(store.deliverable(now).unwrap().is_empty());

    // A snoozed item comes back to the peek at its time, like `deliverable`.
    let wake = now + Duration::minutes(30);
    store
        .feedback(&item.id, Feedback::Later(wake), now)
        .unwrap();
    assert!(store.pending_interrupts(now).unwrap().is_empty());
    assert_eq!(store.pending_interrupts(wake).unwrap().len(), 1);
    assert_eq!(store.deliverable(wake).unwrap().len(), 1);
}
