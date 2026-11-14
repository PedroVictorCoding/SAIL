use sail_flow::schedule::when::{Weekdays, When, describe, next_after, parse};

const A: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z
const LISBON: &str = "Europe/Lisbon";
const NEW_YORK: &str = "America/New_York";

fn spec(text: &str, tz: &str) -> sail_flow::schedule::Spec {
    parse(text, tz, A).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn next(text: &str, tz: &str, after: i64) -> Option<i64> {
    next_after(&spec(text, tz), after)
}

#[test]
fn every_example_parses() {
    for text in [
        "in 30m",
        "in 2 hours",
        "at 2026-11-20 09:00",
        "2026-11-20T09:00",
        "tomorrow at 9am",
        "every 2h",
        "every 15 minutes",
        "every day",
        "every day at 9am",
        "daily at 7am",
        "weekdays at 9am",
        "weekends at 10:30",
        "every monday at 18:30",
        "mon,thu at 8pm",
        "daily at 9am and 6pm",
        "30 9 * * 1-5",
        "@hourly",
        "@daily",
        "@weekly",
        "@monthly",
    ] {
        spec(text, LISBON);
    }
    for time in [
        "9", "9am", "9:05", "09:05", "9.05pm", "21:05", "noon", "midnight",
    ] {
        spec(&format!("daily at {time}"), LISBON);
    }
    for days in [
        "daily",
        "weekdays",
        "workdays",
        "weekends",
        "monday",
        "mondays",
        "mon",
        "mon,thu",
        "monday and thursday",
    ] {
        spec(&format!("{days} at 9am"), LISBON);
    }
}

#[test]
fn parses_the_expected_shapes() {
    assert_eq!(
        spec("in 30m", LISBON),
        sail_flow::schedule::Spec {
            when: When::Once { at: A + 1800 },
            tz: LISBON.into()
        }
    );
    assert_eq!(
        spec("every 2h", LISBON),
        sail_flow::schedule::Spec {
            when: When::Every {
                secs: 7200,
                anchor: A
            },
            tz: LISBON.into()
        }
    );
    match spec("mon,thu at 8pm", LISBON).when {
        When::Weekly { days, times } => {
            assert_eq!(days.0, 0b000_1001);
            assert_eq!(times, vec![(20, 0)]);
        }
        other => panic!("{other:?}"),
    }
    match spec("daily at 9am and 6pm, 7.05pm", LISBON).when {
        When::Weekly { days, times } => {
            assert_eq!(days, Weekdays::ALL);
            assert_eq!(times, vec![(9, 0), (18, 0), (19, 5)]);
        }
        other => panic!("{other:?}"),
    }
    match spec("weekdays at 9am", LISBON).when {
        When::Weekly { days, .. } => assert_eq!(days, Weekdays::WORKDAYS),
        other => panic!("{other:?}"),
    }
    match spec("weekends at 10:30", LISBON).when {
        When::Weekly { days, .. } => assert_eq!(days, Weekdays::WEEKEND),
        other => panic!("{other:?}"),
    }
    match spec("daily at 12am", LISBON).when {
        When::Weekly { times, .. } => assert_eq!(times, vec![(0, 0)]),
        other => panic!("{other:?}"),
    }
    match spec("daily at 12pm", LISBON).when {
        When::Weekly { times, .. } => assert_eq!(times, vec![(12, 0)]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rejects_with_readable_text() {
    let cases = [
        "",
        "   ",
        "evry day",
        "every 1m",
        "every 4 minutes",
        "every 0 minutes",
        "daily at 25:00",
        "daily at 13pm",
        "daily at 9:60",
        "daily at quarter past",
        "daily at",
        "at nonsense",
        "tomorrow at 99pm",
        "in 0m",
        "in 3 lightyears",
        "daily at 9am and 10am and 11am and 12pm and 1pm and 2pm and 3pm and 4pm and 5pm and 6pm and 7pm and 8pm and 9pm",
        "30 9 * *",
        "60 9 * * *",
        "0 24 * * *",
        "0 0 32 * *",
        "0 0 * 13 *",
        "0 0 * * 8",
        "*/0 * * * *",
        "@yearly",
    ];
    for text in cases {
        assert!(parse(text, LISBON, A).is_err(), "{text:?} parsed");
    }
    assert_eq!(
        parse("evry day", LISBON, A).unwrap_err(),
        "I don't understand \"evry day\"; try \"every day at 9am\""
    );
    assert_eq!(
        parse("every 1m", LISBON, A).unwrap_err(),
        "the shortest interval is 5 minutes"
    );
    assert_eq!(
        parse("daily at 9am", "Mars/Olympus", A).unwrap_err(),
        "unknown time zone \"Mars/Olympus\""
    );
    assert_eq!(
        parse("in 0m", LISBON, A).unwrap_err(),
        "choose a time in the future"
    );
    let long = "x".repeat(201);
    assert_eq!(
        parse(&long, LISBON, A).unwrap_err(),
        "the schedule is too long; keep it under 200 characters"
    );
}

#[test]
fn daylight_saving_spring_forward_runs_after_the_gap() {
    // Europe/Lisbon, 2026-03-29: 01:00 local jumps to 02:00 (01:00Z).
    let at = next("at 2026-03-29 01:30", LISBON, 1_774_742_400);
    assert_eq!(at, Some(1_774_746_000));
    let daily = next("daily at 1:30", LISBON, 1_774_742_400);
    assert_eq!(daily, Some(1_774_746_000));

    // America/New_York, 2026-03-08: 02:00 local jumps to 03:00 (07:00Z).
    let at = next("at 2026-03-08 02:30", NEW_YORK, 1_772_928_000);
    assert_eq!(at, Some(1_772_953_200));
    let daily = next("daily at 2:30", NEW_YORK, 1_772_928_000);
    assert_eq!(daily, Some(1_772_953_200));
}

#[test]
fn daylight_saving_fall_back_runs_at_the_earlier_instant() {
    // Europe/Lisbon, 2026-10-25: 02:00 local falls back to 01:00 (00:30Z is the
    // earlier 01:30).
    let at = next("at 2026-10-25 01:30", LISBON, 1_792_886_400);
    assert_eq!(at, Some(1_792_888_200));
    let daily = next("daily at 1:30", LISBON, 1_792_886_400);
    assert_eq!(daily, Some(1_792_888_200));

    // America/New_York, 2026-11-01: 02:00 local falls back to 01:00 (05:30Z is the
    // earlier 01:30).
    let at = next("at 2026-11-01 01:30", NEW_YORK, 1_793_491_200);
    assert_eq!(at, Some(1_793_511_000));
    let daily = next("daily at 1:30", NEW_YORK, 1_793_491_200);
    assert_eq!(daily, Some(1_793_511_000));
}

#[test]
fn cron_day_fields_match_either_when_both_are_restricted() {
    // 13 November 2026 is a Friday. The 13th is not the next Monday, so the
    // day-of-week field decides until the 13th.
    let parsed = spec("0 0 13 * 1", LISBON);
    assert_eq!(next_after(&parsed, 1_793_491_200), Some(1_793_577_600)); // Mon 2 Nov
    let parsed = spec("0 0 1 * 1", LISBON);
    assert_eq!(next_after(&parsed, 1_793_577_600), Some(1_794_182_400)); // Mon 9 Nov
    // If only one day field is restricted, only it decides.
    let parsed = spec("0 0 13 * *", LISBON);
    assert_eq!(next_after(&parsed, 1_793_491_200), Some(1_794_528_000)); // Fri 13 Nov
}

#[test]
fn cron_with_no_slot_in_five_years_returns_none() {
    let parsed = spec("0 0 31 2 *", LISBON);
    assert_eq!(next_after(&parsed, A), None);
}

#[test]
fn monthly_cron_on_the_31st_skips_short_months() {
    let parsed = spec("0 0 31 * *", LISBON);
    assert_eq!(next_after(&parsed, A), Some(1_769_817_600)); // 31 Jan
    assert_eq!(next_after(&parsed, 1_769_817_600), Some(1_774_911_600)); // 31 Mar
}

#[test]
fn a_once_in_the_past_has_no_next_slot() {
    let parsed = parse("at 2020-01-01 09:00", LISBON, A).unwrap();
    assert_eq!(next_after(&parsed, A), None);
}

#[test]
fn describe_is_canonical_and_has_the_zone() {
    assert_eq!(
        describe(&spec("weekdays at 9am", LISBON)),
        "weekdays at 09:00 (Europe/Lisbon)"
    );
    assert_eq!(
        describe(&spec("every 2h", LISBON)),
        "every 2 hours (Europe/Lisbon)"
    );
    assert_eq!(
        describe(&spec("at 2026-11-20 09:00", LISBON)),
        "once on Fri 20 Nov 2026 at 09:00 (Europe/Lisbon)"
    );
    assert_eq!(
        describe(&spec("30 9 * * 1-5", LISBON)),
        "cron 30 9 * * 1-5 (Europe/Lisbon)"
    );
    assert_eq!(
        describe(&spec("daily at 9am and 6pm", LISBON)),
        "every day at 09:00 and 18:00 (Europe/Lisbon)"
    );
    assert_eq!(
        describe(&spec("mon,thu at 8pm", LISBON)),
        "mon, thu at 20:00 (Europe/Lisbon)"
    );
    assert_eq!(
        describe(&spec("weekends at 10:30", LISBON)),
        "weekends at 10:30 (Europe/Lisbon)"
    );
    assert_eq!(
        describe(&spec("@daily", LISBON)),
        "cron 0 0 * * * (Europe/Lisbon)"
    );
    assert_eq!(
        describe(&spec("every day", LISBON)),
        "every day (Europe/Lisbon)"
    );
}

#[test]
fn next_slots_table() {
    // (text, tz, after, expected)
    let rows: &[(&str, &str, i64, Option<i64>)] = &[
        ("in 30m", LISBON, A, Some(A + 1800)),
        ("in 2 hours", LISBON, A, Some(A + 7200)),
        ("in 90m", LISBON, A, Some(A + 5400)),
        ("at 2026-11-20 09:00", LISBON, A, Some(1_795_165_200)),
        ("2026-11-20T09:00", LISBON, A, Some(1_795_165_200)),
        ("every 2h", LISBON, A, Some(A + 7200)),
        ("every 1h", LISBON, A, Some(A + 3600)),
        ("every 15 minutes", LISBON, A + 900, Some(A + 1800)),
        ("every day", LISBON, A, Some(A + 86_400)),
        ("every 5 minutes", LISBON, A + 299, Some(A + 300)),
        ("every 5 minutes", LISBON, A + 301, Some(A + 600)),
        ("daily at 7am", LISBON, 1_794_614_400, Some(1_794_639_600)),
        ("daily at 12am", LISBON, 1_794_614_400, Some(1_794_700_800)),
        ("daily at noon", LISBON, 1_794_614_400, Some(1_794_657_600)),
        (
            "weekdays at 9am",
            LISBON,
            1_794_614_400,
            Some(1_794_819_600),
        ),
        (
            "weekends at 10:30",
            LISBON,
            1_794_657_600,
            Some(1_794_738_600),
        ),
        (
            "every monday at 18:30",
            LISBON,
            1_794_614_400,
            Some(1_794_853_800),
        ),
        ("mon,thu at 8pm", LISBON, 1_794_614_400, Some(1_794_859_200)),
        (
            "daily at 9am and 6pm",
            LISBON,
            1_794_657_600,
            Some(1_794_679_200),
        ),
        ("30 9 * * 1-5", LISBON, 1_794_614_400, Some(1_794_821_400)),
        ("@hourly", LISBON, 1_794_659_640, Some(1_794_661_200)),
        ("@daily", LISBON, 1_794_659_640, Some(1_794_700_800)),
        ("@weekly", LISBON, 1_794_614_400, Some(1_794_700_800)),
        ("@monthly", LISBON, 1_794_614_400, Some(1_796_083_200)),
        ("0 0 13 * 1", LISBON, 1_793_491_200, Some(1_793_577_600)),
        ("0 0 1 * 1", LISBON, 1_793_577_600, Some(1_794_182_400)),
        ("*/15 * * * *", LISBON, 1_794_658_020, Some(1_794_658_500)),
        ("0 9 * jan mon", LISBON, A, Some(1_767_603_600)),
        ("0 0 31 * *", LISBON, A, Some(1_769_817_600)),
        ("0 0 31 2 *", LISBON, A, None),
    ];
    let tomorrow = parse("tomorrow at 9am", LISBON, 1_794_657_600).unwrap();
    assert_eq!(next_after(&tomorrow, 1_794_657_600), Some(1_794_733_200));
    for (text, tz, after, expected) in rows {
        assert_eq!(next(text, tz, *after), *expected, "{text} after {after}");
    }
}

#[test]
fn functions_are_deterministic() {
    let first = parse("mon,thu at 8pm", LISBON, A).unwrap();
    let second = parse("mon,thu at 8pm", LISBON, A).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        next_after(&first, 1_794_614_400),
        next_after(&second, 1_794_614_400)
    );
    assert_eq!(describe(&first), describe(&second));
}

#[test]
fn a_year_of_five_minute_slots_is_fast_in_debug() {
    let start = std::time::Instant::now();
    let parsed = spec("every 5 minutes", LISBON);
    let mut slot = A;
    for _ in 0..(365 * 24 * 12) {
        slot = next_after(&parsed, slot).expect("slot");
    }
    assert!(
        start.elapsed() < std::time::Duration::from_millis(200),
        "walk took {:?}",
        start.elapsed()
    );
}
