use chrono::{DateTime, Duration, TimeZone, Utc};
use sail_pulse::attention::{
    AttentionContext, AttentionSettings, Autonomy, Delivery, DropReason, Feedback, Preference,
    Provenance, Signal, Source,
    score::{assess, deadline_pressure, interruption_cost, learn},
};

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap()
}

fn base_signal(now: DateTime<Utc>) -> Signal {
    Signal {
        source: Source::Moodle,
        kind: "deadline".into(),
        dedupe_key: "moodle:exam:1".into(),
        title: "Physics midterm".into(),
        body: "Starts soon".into(),
        urgency: 0.7,
        relevance: 0.8,
        novelty: 0.7,
        confidence: 0.9,
        deadline: None,
        provenance: Provenance::Observation,
        link: None,
        observed_at: now,
    }
}

fn base_context(now: DateTime<Utc>) -> AttentionContext {
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
fn deadline_pressure_curve_and_stale() {
    let now = base_time();
    assert_eq!(deadline_pressure(None, now), 0.0);
    assert_eq!(
        deadline_pressure(Some(now - Duration::minutes(1)), now),
        0.0
    );
    assert_eq!(deadline_pressure(Some(now + Duration::hours(1)), now), 1.0);
    assert_eq!(deadline_pressure(Some(now + Duration::hours(2)), now), 1.0);
    assert_eq!(deadline_pressure(Some(now + Duration::days(7)), now), 0.0);
    assert_eq!(deadline_pressure(Some(now + Duration::days(10)), now), 0.0);

    let mid = deadline_pressure(Some(now + Duration::days(3) + Duration::hours(13)), now);
    assert!(mid > 0.45 && mid < 0.55, "expected ~0.5, got {mid}");
}

#[test]
fn interruption_cost_precedence() {
    let now = base_time();
    let mut ctx = base_context(now);
    assert!((interruption_cost(&ctx) - 0.2).abs() < 1e-6);

    ctx.busy = true;
    assert!((interruption_cost(&ctx) - 0.6).abs() < 1e-6);

    ctx.voice_active = true;
    assert!((interruption_cost(&ctx) - 0.6).abs() < 1e-6);

    ctx.focus_until = Some(now + Duration::minutes(30));
    assert!((interruption_cost(&ctx) - 0.9).abs() < 1e-6);

    ctx.quiet_hours = true;
    assert!((interruption_cost(&ctx) - 1.0).abs() < 1e-6);
}

#[test]
fn low_urgency_in_focus_goes_to_digest_not_interrupt() {
    let now = base_time();
    let mut sig = base_signal(now);
    sig.urgency = 0.2;
    sig.relevance = 0.4;
    sig.novelty = 0.4;
    sig.confidence = 0.4;

    let mut ctx = base_context(now);
    ctx.focus_until = Some(now + Duration::hours(1));

    let res = assess(&sig, &ctx, &AttentionSettings::default(), None, false);
    assert_eq!(res.delivery, Delivery::Digest, "{res:?}");
}

#[test]
fn quiet_hours_allow_imminent_commitment_but_block_observation() {
    let now = base_time();
    let mut ctx = base_context(now);
    ctx.quiet_hours = true;

    let mut commitment = base_signal(now);
    commitment.provenance = Provenance::UserCommitment;
    commitment.urgency = 0.96;
    commitment.relevance = 0.95;
    commitment.novelty = 0.9;
    commitment.confidence = 0.95;
    commitment.deadline = Some(now + Duration::minutes(90));

    let res_commit = assess(
        &commitment,
        &ctx,
        &AttentionSettings::default(),
        None,
        false,
    );
    assert_eq!(res_commit.delivery, Delivery::Interrupt, "{res_commit:?}");

    let mut observation = commitment.clone();
    observation.provenance = Provenance::Observation;
    let res_obs = assess(
        &observation,
        &ctx,
        &AttentionSettings::default(),
        None,
        false,
    );
    assert_ne!(res_obs.delivery, Delivery::Interrupt, "{res_obs:?}");
}

#[test]
fn autonomy_off_quiet_and_proactive_modifiers() {
    let now = base_time();
    let ctx = base_context(now);

    let off = AttentionSettings {
        autonomy: Autonomy::Off,
        ..Default::default()
    };

    let obs = base_signal(now);
    assert_eq!(
        assess(&obs, &ctx, &off, None, false).delivery,
        Delivery::Drop(DropReason::AutonomyOff)
    );

    let mut commit = obs.clone();
    commit.provenance = Provenance::UserCommitment;
    assert_eq!(
        assess(&commit, &ctx, &off, None, false).delivery,
        Delivery::Digest
    );

    let quiet = AttentionSettings {
        autonomy: Autonomy::Quiet,
        ..Default::default()
    };
    let mut high = base_signal(now);
    high.urgency = 1.0;
    high.relevance = 1.0;
    high.novelty = 1.0;
    high.confidence = 1.0;
    assert_eq!(
        assess(&high, &ctx, &quiet, None, false).delivery,
        Delivery::Digest
    );

    // Balanced would only Suggest (score ~0.725, 0.725 - 0.2 = 0.525 < 0.6),
    // while Proactive lowers interrupt_threshold to 0.5 so it Interrupts.
    let mut borderline = base_signal(now);
    borderline.urgency = 0.75;
    borderline.relevance = 0.75;
    borderline.novelty = 0.7;
    borderline.confidence = 0.7;
    let balanced = AttentionSettings::default();
    let proactive = AttentionSettings {
        autonomy: Autonomy::Proactive,
        ..Default::default()
    };

    assert_eq!(
        assess(&borderline, &ctx, &balanced, None, false).delivery,
        Delivery::Suggest
    );
    assert_eq!(
        assess(&borderline, &ctx, &proactive, None, false).delivery,
        Delivery::Interrupt
    );
}

#[test]
fn stale_deadline_drops_unless_user_commitment() {
    let now = base_time();
    let ctx = base_context(now);
    let settings = AttentionSettings::default();

    let mut obs = base_signal(now);
    obs.deadline = Some(now - Duration::hours(1));
    assert_eq!(
        assess(&obs, &ctx, &settings, None, false).delivery,
        Delivery::Drop(DropReason::Stale)
    );

    let mut commit = obs;
    commit.provenance = Provenance::UserCommitment;
    assert_eq!(
        assess(&commit, &ctx, &settings, None, false).delivery,
        Delivery::Digest
    );
}

#[test]
fn learned_multiplier_stays_within_bounds_and_decays() {
    let now = base_time();
    let ctx = base_context(now);
    let settings = AttentionSettings::default();

    let mut sig = base_signal(now);
    sig.urgency = 0.5;
    sig.relevance = 0.5;
    sig.novelty = 0.5;
    sig.confidence = 0.5;
    let neutral = assess(&sig, &ctx, &settings, None, false).score;

    let mut pref_good = Preference {
        source: Source::Moodle,
        kind: "deadline".into(),
        accepted: 0.0,
        rejected: 0.0,
        muted: false,
        updated_at: now,
    };
    for _ in 0..500 {
        pref_good = learn(pref_good, Feedback::Useful, now);
    }
    let boosted = assess(&sig, &ctx, &settings, Some(&pref_good), false).score;
    assert!(
        boosted <= neutral * 1.4 + 1e-4 && boosted >= neutral * 1.38,
        "neutral={neutral}, boosted={boosted}"
    );

    let mut pref_bad = Preference {
        source: Source::Moodle,
        kind: "deadline".into(),
        accepted: 0.0,
        rejected: 0.0,
        muted: false,
        updated_at: now,
    };
    for _ in 0..500 {
        pref_bad = learn(pref_bad, Feedback::Dismiss, now);
    }
    let penalized = assess(&sig, &ctx, &settings, Some(&pref_bad), false).score;
    assert!(
        penalized >= neutral * 0.6 - 1e-4 && penalized <= neutral * 0.62,
        "neutral={neutral}, penalized={penalized}"
    );

    // Decay halves tallies after 30 days.
    let pref_decay = Preference {
        source: Source::Moodle,
        kind: "deadline".into(),
        accepted: 10.0,
        rejected: 4.0,
        muted: false,
        updated_at: now,
    };
    let after_30d = learn(pref_decay, Feedback::NotNow, now + Duration::days(30));
    assert!((after_30d.accepted - 5.0).abs() < 1e-4, "{after_30d:?}");
    assert!((after_30d.rejected - 2.25).abs() < 1e-4, "{after_30d:?}");
}
