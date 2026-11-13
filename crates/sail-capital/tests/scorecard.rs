//! The one track record: standings at their exact sample boundaries, names, order,
//! summary and JSON shape. All data is synthetic.
use chrono::{DateTime, TimeZone, Utc};
use sail_capital::forecast::{ModelId, ModelScore};
use sail_capital::outlook::TrackRecord;
use sail_capital::scorecard::{Scorecard, ScorecardInputs, Standing, summarize};
use sail_capital::service::{CapitalService, RecordTotals};
use sail_capital::signals::{MIN_ORIGINS, SignalId, SignalSkill};
use sail_capital::trade::{Evidence, MIN_TRADES, SetupId, Side, Verdict};

const CASH: &[u8] = include_bytes!("fixtures/xtb/english_cash.csv");

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 13, 9, 0, 0).unwrap()
}

fn score(
    model: ModelId,
    horizon: u32,
    forecasts: u32,
    calibrated: bool,
    skill: f64,
    interval_90: f64,
) -> ModelScore {
    ModelScore {
        model,
        horizon_sessions: horizon,
        forecasts,
        crps: 0.0,
        pinball: 0.0,
        nll: None,
        crps_skill: skill,
        coverage: Vec::new(),
        interval_90,
        direction_hit_rate: 0.0,
        calibrated,
    }
}

fn signal(
    signal: SignalId,
    horizon: u32,
    origins: u32,
    ic_mean: f64,
    t_stat: f64,
    weight: f64,
) -> SignalSkill {
    SignalSkill {
        signal,
        horizon_sessions: horizon,
        origins,
        ic_mean,
        ic_se: 0.0,
        t_stat,
        ic_used: 0.0,
        weight,
    }
}

fn track(horizon: u32, replays: u32, inside_90: f64, skill: f64) -> TrackRecord {
    TrackRecord {
        horizon,
        replays,
        inside_68: 0.68,
        inside_90,
        mean_percentile: 50.0,
        skill_vs_no_change: skill,
        direction_hit_rate: 0.5,
    }
}

fn evidence(setup: SetupId, side: Side, trades: u32) -> Evidence {
    Evidence {
        setup,
        side,
        trades,
        hit_target1: 0.5,
        hit_target1_ci: (0.4, 0.6),
        expectancy_r: 0.1,
        expectancy_ci: (0.0, 0.2),
        profit_factor: 1.2,
        max_drawdown_r: 1.0,
        mean_sessions: 5.0,
    }
}

fn verdict(
    setup: SetupId,
    side: Side,
    group: &str,
    trades: u32,
    publishes: bool,
    reason: &str,
) -> Verdict {
    Verdict {
        group: group.to_string(),
        selection: evidence(setup, side, trades),
        holdout: evidence(setup, side, trades / 2),
        publishes,
        candidate: false,
        reason: reason.to_string(),
    }
}

fn totals(closed: u32, mean_r: f64) -> RecordTotals {
    RecordTotals {
        closed,
        hit_target1: 0.58,
        mean_r,
        cumulative_r: mean_r * closed as f64,
    }
}

fn summarize_all<'a>(
    forecasts: &'a [ModelScore],
    signals: &'a [SignalSkill],
    outlook: &'a [TrackRecord],
    setups: &'a [Verdict],
    live: Option<&'a RecordTotals>,
) -> Scorecard {
    summarize(
        &ScorecardInputs {
            forecasts,
            signals,
            outlook,
            setups,
            live,
        },
        now(),
    )
}

fn row<'a>(card: &'a Scorecard, key: &str) -> &'a sail_capital::scorecard::ScoreRow {
    card.rows
        .iter()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("no row {key:?} in {:?}", card.rows))
}

#[test]
fn forecast_stands_at_100_and_99_forecasts() {
    let forecasts = vec![
        score(ModelId::TrailingDrift, 21, 100, true, 0.12, 0.90),
        score(ModelId::EwmaVolatility, 21, 99, true, 0.12, 0.90),
        score(ModelId::EmpiricalHorizon, 21, 100, false, 0.12, 0.78),
        score(ModelId::NoChange, 21, 1000, true, 0.0, 0.90),
    ];
    let card = summarize_all(&forecasts, &[], &[], &[], None);

    assert_eq!(
        row(&card, "forecast:trailing_drift:h21").standing,
        Standing::Earned
    );
    assert_eq!(
        row(&card, "forecast:ewma_volatility:h21").standing,
        Standing::NotYet
    );
    assert_eq!(
        row(&card, "forecast:empirical_horizon:h21").standing,
        Standing::Failed
    );
    assert!(!card.rows.iter().any(|row| row.key.contains("no_change")));
    assert_eq!(
        row(&card, "forecast:trailing_drift:h21").headline,
        "12% better than no change over 100 forecasts; ranges well calibrated."
    );
    assert_eq!(
        row(&card, "forecast:empirical_horizon:h21").headline,
        "Ranges too narrow: 78% landed inside the 90% band."
    );
}

#[test]
fn signal_stands_at_min_origins() {
    let signals = vec![
        signal(
            SignalId::Momentum12_1,
            20,
            MIN_ORIGINS as u32,
            0.04,
            2.3,
            0.31,
        ),
        signal(
            SignalId::ShortTermReversal,
            20,
            (MIN_ORIGINS - 1) as u32,
            0.04,
            2.3,
            0.0,
        ),
        signal(
            SignalId::LowVolatility,
            20,
            MIN_ORIGINS as u32,
            0.01,
            0.4,
            0.0,
        ),
    ];
    let card = summarize_all(&[], &signals, &[], &[], None);

    assert_eq!(
        row(&card, "signal:momentum12_1:h20").standing,
        Standing::Earned
    );
    assert_eq!(
        row(&card, "signal:short_term_reversal:h20").standing,
        Standing::NotYet
    );
    assert_eq!(
        row(&card, "signal:low_volatility:h20").standing,
        Standing::Failed
    );
    assert_eq!(
        row(&card, "signal:momentum12_1:h20").headline,
        "Rank IC +0.04 (t 2.3) over 26 weeks; weight 0.31."
    );
}

#[test]
fn outlook_stands_at_24_replays() {
    let outlook = vec![
        track(21, 24, 0.88, 0.03),
        track(63, 23, 0.88, 0.03),
        track(126, 24, 0.90, -0.01),
    ];
    let card = summarize_all(&[], &[], &outlook, &[], None);

    assert_eq!(row(&card, "outlook:h21").standing, Standing::Earned);
    assert_eq!(row(&card, "outlook:h63").standing, Standing::NotYet);
    assert_eq!(row(&card, "outlook:h126").standing, Standing::Failed);
    assert_eq!(row(&card, "outlook:h21").name, "Blended outlook");
    assert_eq!(
        row(&card, "outlook:h21").headline,
        "90% band held 88% of 24 monthly replays; 3% better than no change."
    );
}

#[test]
fn setups_stand_at_min_trades() {
    let setups = vec![
        verdict(
            SetupId::TrendPullback,
            Side::Long,
            "stocks",
            MIN_TRADES,
            true,
            "Publishes.",
        ),
        verdict(
            SetupId::SqueezeBreakout,
            Side::Short,
            "fx",
            MIN_TRADES - 1,
            false,
            "Too few trades.",
        ),
        verdict(
            SetupId::Donchian55,
            Side::Long,
            "indices",
            MIN_TRADES,
            false,
            "Below zero after costs.",
        ),
    ];
    let card = summarize_all(&[], &[], &[], &setups, None);

    assert_eq!(
        row(&card, "setup:trend_pullback:long:stocks").standing,
        Standing::Earned
    );
    assert_eq!(
        row(&card, "setup:squeeze_breakout:short:fx").standing,
        Standing::NotYet
    );
    assert_eq!(
        row(&card, "setup:donchian55:long:indices").standing,
        Standing::Failed
    );
    assert_eq!(
        row(&card, "setup:squeeze_breakout:short:fx").headline,
        "Needs 100 trades; has 99."
    );
    assert_eq!(
        row(&card, "setup:donchian55:long:indices").headline,
        "Below zero after costs."
    );
    assert_eq!(
        row(&card, "setup:trend_pullback:long:stocks").name,
        "Trend pullback · long · stocks"
    );
}

#[test]
fn live_stands_at_20_closed_signals() {
    let earned = totals(20, 0.21);
    let not_yet = totals(19, 0.21);
    let failed = totals(20, 0.0);

    let earned_card = summarize_all(&[], &[], &[], &[], Some(&earned));
    let not_yet_card = summarize_all(&[], &[], &[], &[], Some(&not_yet));
    let failed_card = summarize_all(&[], &[], &[], &[], Some(&failed));

    assert_eq!(row(&earned_card, "live").standing, Standing::Earned);
    assert_eq!(row(&earned_card, "live").name, "Published signals (live)");
    assert_eq!(
        row(&earned_card, "live").headline,
        "20 closed: hit target 1 in 58%, mean +0.21R."
    );
    assert_eq!(row(&not_yet_card, "live").standing, Standing::NotYet);
    assert_eq!(row(&failed_card, "live").standing, Standing::Failed);
}

#[test]
fn rows_are_ordered_earned_then_not_yet_then_failed() {
    let forecasts = vec![score(ModelId::TrailingDrift, 21, 100, true, 0.12, 0.90)];
    let signals = vec![signal(
        SignalId::Momentum12_1,
        20,
        MIN_ORIGINS as u32,
        0.04,
        2.3,
        0.31,
    )];
    let outlook = vec![track(21, 24, 0.88, 0.03)];
    let setups = vec![verdict(
        SetupId::TrendPullback,
        Side::Long,
        "stocks",
        MIN_TRADES - 1,
        false,
        "Too few trades.",
    )];
    let failed = totals(20, -0.1);

    let card = summarize_all(&forecasts, &signals, &outlook, &setups, Some(&failed));

    let standing: Vec<Standing> = card.rows.iter().map(|row| row.standing).collect();
    assert_eq!(
        standing,
        vec![
            Standing::Earned,
            Standing::Earned,
            Standing::Earned,
            Standing::NotYet,
            Standing::Failed,
        ]
    );
    let names: Vec<&str> = card.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Trailing drift",
            "12-1 momentum",
            "Blended outlook",
            "Trend pullback · long · stocks",
            "Published signals (live)",
        ]
    );
}

#[test]
fn within_a_standing_horizons_ascend() {
    let forecasts = vec![
        score(ModelId::TrailingDrift, 63, 100, true, 0.12, 0.90),
        score(ModelId::TrailingDrift, 21, 100, true, 0.12, 0.90),
    ];
    let card = summarize_all(&forecasts, &[], &[], &[], None);
    assert_eq!(
        card.rows
            .iter()
            .map(|row| row.horizon_sessions)
            .collect::<Vec<_>>(),
        vec![Some(21), Some(63)]
    );
}

#[test]
fn summary_names_the_first_three_earned_rows() {
    let forecasts = vec![
        score(ModelId::TrailingDrift, 21, 100, true, 0.12, 0.90),
        score(ModelId::EwmaVolatility, 21, 100, true, 0.08, 0.90),
    ];
    let signals = vec![
        signal(
            SignalId::Momentum12_1,
            20,
            MIN_ORIGINS as u32,
            0.04,
            2.3,
            0.31,
        ),
        signal(SignalId::Trend, 20, MIN_ORIGINS as u32, 0.02, 2.1, 0.12),
    ];
    let card = summarize_all(&forecasts, &signals, &[], &[], None);

    assert_eq!(card.earned, 4);
    assert_eq!(
        card.summary,
        "4 of 4 earned trust: EWMA volatility, Trailing drift, 12-1 momentum."
    );
}

#[test]
fn an_empty_card_says_nothing_has_earned_trust() {
    let card = summarize_all(&[], &[], &[], &[], None);
    assert!(card.rows.is_empty());
    assert_eq!(card.earned, 0);
    assert_eq!(
        card.summary,
        "Nothing has earned trust yet. SAIL shows ranges, not calls."
    );
    assert_eq!(card.as_of, now());
}

#[test]
fn json_uses_plain_snake_case_strings() {
    let forecasts = vec![score(ModelId::EwmaVolatility, 21, 99, true, 0.12, 0.90)];
    let card = summarize_all(&forecasts, &[], &[], &[], None);
    let value = serde_json::to_value(&card).unwrap();
    let row = &value["rows"][0];
    assert_eq!(row["family"], serde_json::json!("forecast"));
    assert_eq!(row["standing"], serde_json::json!("not_yet"));
    assert!(row["family"].is_string());
    assert!(row["standing"].is_string());
}

#[test]
fn a_fresh_store_has_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let service = CapitalService::open(dir.path()).unwrap();
    service
        .import_xtb(None, "cash.csv", CASH, None, None, now())
        .unwrap();
    let account = service.resolve_account(None).unwrap();

    let card = service.scorecard(&account).unwrap();

    assert!(card.rows.is_empty(), "{:?}", card.rows);
    assert_eq!(card.earned, 0);
    assert_eq!(
        card.summary,
        "Nothing has earned trust yet. SAIL shows ranges, not calls."
    );
}
