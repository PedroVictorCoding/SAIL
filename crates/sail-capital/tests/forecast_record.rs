//! Round 20 frozen baseline forecast records and maturity scoring.

use chrono::{Days, NaiveDate, TimeZone, Utc};
use sail_capital::contracts::{DecisionTime, InputProvenance};
use sail_capital::forecast::baseline::features;
use sail_capital::forecast::record::{
    BASELINE_MODEL_VERSION, ForecastRecord, RealizedOutcome, ScoreStatus, score,
};
use sail_capital::forecast::{CalibrationState, CloseSeries, HORIZONS, ModelId, forecast};
use sail_capital::store::CapitalStore;
use sail_capital::trade::Bar;

fn day(index: u64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 1, 1).unwrap() + Days::new(index)
}

fn bars(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|index| {
            let close = 100.0 + index as f64 * 0.05;
            Bar {
                session: day(index as u64),
                open: close,
                high: close * 1.01,
                low: close * 0.99,
                close,
                volume: None,
                dividend: None,
            }
        })
        .collect()
}

fn make_record(count: usize, horizon: u32) -> ForecastRecord {
    let bars = bars(count);
    let features = features(&bars).expect("enough bars");
    let closes: Vec<_> = bars
        .iter()
        .map(|bar| {
            (
                bar.session,
                rust_decimal::Decimal::from_f64_retain(bar.close).unwrap(),
            )
        })
        .collect();
    let distribution = forecast(
        &CloseSeries {
            instrument_key: "sym:TEST",
            symbol: "TEST.US",
            closes: &closes,
        },
        ModelId::EwmaVolatility,
        horizon,
        CalibrationState {
            verified: false,
            cohort: None,
        },
    );
    ForecastRecord::new(
        DecisionTime::new(
            Utc.from_utc_datetime(&day((count - 1) as u64).and_hms_opt(22, 0, 0).unwrap()),
            day((count - 1) as u64),
        ),
        "sym:TEST",
        "TEST.US",
        horizon,
        BASELINE_MODEL_VERSION,
        features,
        distribution,
        "manifest:abc",
        InputProvenance::Observed,
    )
    .expect("a valid record")
}

#[test]
fn a_record_is_content_addressed_before_any_outcome() {
    let first = make_record(260, HORIZONS[2]);
    let again = make_record(260, HORIZONS[2]);
    assert_eq!(first.record_id, again.record_id);
    assert_eq!(first, again);
    assert_eq!(first.features.bars, 260);
    assert_eq!(first.distribution.horizon_sessions, HORIZONS[2]);
    assert_eq!(first.provenance, InputProvenance::Observed);
}

#[test]
fn the_store_keeps_one_row_per_record_id_and_a_correction_gets_a_new_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = CapitalStore::open(dir.path()).unwrap();
    let first = make_record(260, HORIZONS[2]);
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    assert!(store.record_forecast(&first, now).unwrap());
    assert!(
        !store
            .record_forecast(&first, now + chrono::Duration::hours(1))
            .unwrap()
    );
    assert_eq!(
        store
            .forecast_record(&first.record_id)
            .unwrap()
            .expect("stored"),
        first
    );
    assert_eq!(
        store
            .forecast_record_id(
                &first.instrument_key,
                first.horizon_sessions,
                first.decision.at
            )
            .unwrap(),
        Some(first.record_id.clone())
    );

    let corrected = ForecastRecord::new(
        first.decision,
        &first.instrument_key,
        &first.symbol,
        first.horizon_sessions,
        BASELINE_MODEL_VERSION,
        first.features.clone(),
        first.distribution.clone(),
        "manifest:corrected",
        InputProvenance::Observed,
    )
    .unwrap();
    assert_ne!(corrected.record_id, first.record_id);
    assert!(store.record_forecast(&corrected, now).unwrap());
    let rows = store
        .forecast_records_for(&first.instrument_key, first.horizon_sessions)
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], corrected);
}

#[test]
fn scoring_uses_next_open_to_horizon_close_with_actions_and_delisting() {
    let record = make_record(260, HORIZONS[1]);
    let entry = record.decision.session;
    let exit = entry + Days::new(21);
    let complete = score(
        &record,
        &RealizedOutcome {
            entry_session: entry,
            exit_session: exit,
            entry_open: 100.0,
            exit_close: 100.1,
            split_factor: 1.0,
            cash_per_share: 0.0,
            horizon_complete: true,
            delisted: false,
        },
    )
    .unwrap();
    assert_eq!(complete.status, ScoreStatus::Complete);
    assert!((complete.realized_log_return - 1.001f64.ln()).abs() < 1e-12);
    assert!((complete.forecast_error.unwrap() + 1.001f64.ln()).abs() < 1e-12);
    assert_eq!(complete.inside_90, Some(true));
    assert_eq!(complete.inside_50, Some(true));

    let with_actions = score(
        &record,
        &RealizedOutcome {
            entry_session: entry,
            exit_session: exit,
            entry_open: 100.0,
            exit_close: 100.0,
            split_factor: 1.0,
            cash_per_share: 2.0,
            horizon_complete: true,
            delisted: false,
        },
    )
    .unwrap();
    assert!((with_actions.realized_log_return - 1.02f64.ln()).abs() < 1e-12);

    let delisted = score(
        &record,
        &RealizedOutcome {
            entry_session: entry,
            exit_session: entry + Days::new(3),
            entry_open: 100.0,
            exit_close: 0.0,
            split_factor: 1.0,
            cash_per_share: 110.0,
            horizon_complete: false,
            delisted: true,
        },
    )
    .unwrap();
    assert_eq!(delisted.status, ScoreStatus::Delisted);
    assert!((delisted.realized_log_return - 1.1f64.ln()).abs() < 1e-12);
}

#[test]
fn a_mismatched_distribution_or_bad_outcome_is_refused() {
    let mut record = make_record(260, HORIZONS[0]);
    record.distribution.horizon_sessions = 21;
    assert!(record.verify_id().is_err());

    let valid = make_record(260, HORIZONS[0]);
    let outcome = RealizedOutcome {
        entry_session: valid.decision.session,
        exit_session: valid.decision.session - Days::new(1),
        entry_open: 100.0,
        exit_close: 101.0,
        split_factor: 1.0,
        cash_per_share: 0.0,
        horizon_complete: true,
        delisted: false,
    };
    assert!(score(&valid, &outcome).is_err());
}
