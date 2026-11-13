//! Signal v2 strict point-in-time replay regression fixtures (synthetic, no network, temp SQLite).

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use sail_capital::calendar::{ExchangeCalendar, HolidayCalendar, WeekdayCalendar};
use sail_capital::contracts::{AccountId, Currency, DecisionTime, InputProvenance, VintagePolicy};
use sail_capital::dividends::Payment;
use sail_capital::forecast::ModelId;
use sail_capital::market::{Adjustment, DisabledSource, PriceBar, PriceSeries};
use sail_capital::models::{outlook_specs, specs};
use sail_capital::outlook::{ComponentId, PathQuantiles};
use sail_capital::service::CapitalService;
use sail_capital::store::CapitalStore;
use sail_capital::trade::AssetClass;
use sail_capital::universe::{ResearchMember, WatchChange, WatchItem};
use std::sync::Arc;

const XTB_FILE: &[u8] = include_bytes!("fixtures/xtb/xtb_2026_layout.xlsx");

fn ts(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, hh, mm, ss).unwrap()
}

fn d(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn synthetic_bars(start: NaiveDate, count: usize, base_cents: i64) -> Vec<PriceBar> {
    let mut day = start;
    let mut bars = Vec::with_capacity(count);
    let mut i = 0i64;
    while bars.len() < count {
        if day.weekday().number_from_monday() <= 5 {
            let drift = i * 4;
            let wiggle = ((i * 7) % 19) * 3;
            let close = Decimal::new(base_cents + drift + wiggle, 2);
            bars.push(PriceBar {
                session: day,
                open: Some(close - Decimal::new(5, 2)),
                high: Some(close + Decimal::new(15, 2)),
                low: Some(close - Decimal::new(15, 2)),
                close,
                volume: Some(Decimal::from(100_000 + i * 100)),
            });
            i += 1;
        }
        day += Duration::days(1);
    }
    bars
}

fn make_service(dir: &std::path::Path) -> (Arc<CapitalStore>, CapitalService, AccountId) {
    let model_specs: Vec<_> = specs()
        .into_iter()
        .filter(|s| s.name == "chronos")
        .collect();
    let mut out_specs: Vec<_> = outlook_specs()
        .into_iter()
        .filter(|s| s.component == ComponentId::Chronos)
        .collect();
    for s in &mut out_specs {
        s.evaluate_from = d(2024, 1, 1);
    }
    let svc = CapitalService::open(dir)
        .unwrap()
        .with_prices(Arc::new(DisabledSource), false)
        .with_models(model_specs)
        .with_outlook_models(out_specs);
    let store = Arc::new(CapitalStore::open(dir).unwrap());
    let batch = svc
        .import_xtb(
            None,
            "xtb.xlsx",
            XTB_FILE,
            None,
            None,
            ts(2026, 4, 1, 12, 0, 0),
        )
        .unwrap();
    (store, svc, batch.account)
}

#[test]
fn replay_at_t_ignores_later_price_correction_membership_corporate_actions_and_model_output() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, svc, account) = make_service(tmp.path());

    // Pick a held instrument from the imported XTB fixture.
    let projection = svc
        .portfolio(
            &account,
            sail_capital::AsOf {
                occurred_by: ts(2026, 4, 1, 12, 0, 0),
                known_by: ts(2026, 4, 1, 12, 0, 0),
            },
        )
        .unwrap();
    let pos = projection.positions.first().expect("fixture has positions");
    let held_key = pos.instrument.key();
    let held_symbol = pos.instrument.symbol.clone();

    let bars_t = synthetic_bars(d(2025, 1, 2), 360, 10_000);
    let session_t = bars_t.last().unwrap().session;
    let t =
        DateTime::<Utc>::from_naive_utc_and_offset(session_t.and_hms_opt(22, 0, 0).unwrap(), Utc);

    store
        .record_prices(
            &held_key,
            &PriceSeries {
                source_id: "synth".into(),
                symbol: held_symbol.clone(),
                source_symbol: held_symbol.clone(),
                currency: Some(Currency::parse("USD").unwrap()),
                adjustment: Adjustment::SplitAdjusted,
                bars: bars_t.clone(),
                payload_sha256: "v1".into(),
            },
            t,
        )
        .unwrap();

    // Also seed a research membership snapshot, dividend snapshot, earnings snapshot,
    // watchlist change, signal values (v1), and outlook paths (v1) at T.
    store
        .record_research_members(
            "sp500",
            &[ResearchMember {
                yahoo: "AAPL".into(),
                name: "Apple".into(),
                class: AssetClass::Stock,
                membership: vec![(d(2020, 1, 1), None)],
            }],
            t,
        )
        .unwrap();
    let yahoo =
        sail_capital::market::yahoo_symbol(&held_symbol).unwrap_or_else(|| held_symbol.clone());
    store
        .record_dividend_history(
            &yahoo,
            Some("USD"),
            &[Payment {
                ex_date: d(2025, 2, 10),
                per_share: 0.25,
            }],
            t,
        )
        .unwrap();
    store.record_earnings(&yahoo, &[d(2025, 1, 30)], t).unwrap();
    store
        .record_watch_change(
            &WatchChange::Add {
                item: WatchItem {
                    symbol: "SYNTH.US".into(),
                    yahoo: "SYNTH".into(),
                    class: AssetClass::Stock,
                    currency: "USD".into(),
                    name: "Synthetic".into(),
                },
            },
            t,
        )
        .unwrap();
    store
        .record_signal_values(
            "foundation_forecast",
            "chronos-v1",
            &[(held_key.clone(), session_t, 0.015)],
            t,
        )
        .unwrap();
    store
        .record_outlook_paths(
            "chronos-v1",
            &[sail_capital::outlook::ExternalPath {
                component: ComponentId::Chronos,
                instrument_key: held_key.clone(),
                origin: session_t,
                horizon: 21,
                path: PathQuantiles {
                    steps: vec![5, 10, 15, 20, 21],
                    values: vec![[-0.02, -0.01, 0.005, 0.02, 0.03]; 5],
                },
            }],
            t,
        )
        .unwrap();

    let decision_t = DecisionTime::new(t, session_t);
    let outlook_1 = svc
        .outlook_as_of(
            &account,
            &held_symbol,
            21,
            decision_t,
            &WeekdayCalendar,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();
    let forecast_1 = svc
        .forecasts_as_of(
            &account,
            20,
            ModelId::TrailingDrift,
            decision_t,
            &WeekdayCalendar,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();
    let signals_1 = svc
        .signals_as_of(&account, decision_t, VintagePolicy::ObservedOnly)
        .unwrap();

    assert_eq!(outlook_1.manifest.provenance, InputProvenance::Observed);
    assert_eq!(forecast_1.manifest.provenance, InputProvenance::Observed);

    // At T2 > T, append:
    // 1. A corrected close for session_t (and 10 new future bars)
    // 2. A later research membership snapshot (adding a member and closing AAPL)
    // 3. A later dividend & earnings snapshot
    // 4. A later watchlist removal
    // 5. A later model output (v2)
    let t2 = t + Duration::days(10);
    let mut bars_t2 = synthetic_bars(d(2025, 1, 2), 370, 10_000);
    // Deliberately alter the close at session_t to simulate a restatement/correction!
    let idx_t = bars_t2.iter().position(|b| b.session == session_t).unwrap();
    bars_t2[idx_t].close += Decimal::new(2_500, 2);
    let session_t2 = bars_t2.last().unwrap().session;

    store
        .record_prices(
            &held_key,
            &PriceSeries {
                source_id: "synth".into(),
                symbol: held_symbol.clone(),
                source_symbol: held_symbol.clone(),
                currency: Some(Currency::parse("USD").unwrap()),
                adjustment: Adjustment::SplitAdjusted,
                bars: bars_t2,
                payload_sha256: "v2".into(),
            },
            t2,
        )
        .unwrap();
    store
        .record_research_members(
            "sp500",
            &[
                ResearchMember {
                    yahoo: "AAPL".into(),
                    name: "Apple".into(),
                    class: AssetClass::Stock,
                    membership: vec![(d(2020, 1, 1), Some(session_t2))],
                },
                ResearchMember {
                    yahoo: "NEWCO".into(),
                    name: "NewCo".into(),
                    class: AssetClass::Stock,
                    membership: vec![(session_t2, None)],
                },
            ],
            t2,
        )
        .unwrap();
    store
        .record_dividend_history(
            &yahoo,
            Some("USD"),
            &[
                Payment {
                    ex_date: d(2025, 2, 10),
                    per_share: 0.25,
                },
                Payment {
                    ex_date: session_t2,
                    per_share: 0.50,
                },
            ],
            t2,
        )
        .unwrap();
    store
        .record_earnings(&yahoo, &[d(2025, 1, 30), session_t2], t2)
        .unwrap();
    store
        .record_watch_change(
            &WatchChange::Remove {
                symbol: "SYNTH.US".into(),
            },
            t2,
        )
        .unwrap();
    store
        .record_signal_values(
            "foundation_forecast",
            "chronos-v2",
            &[(held_key.clone(), session_t2, -0.045)],
            t2,
        )
        .unwrap();
    store
        .record_outlook_paths(
            "chronos-v2",
            &[sail_capital::outlook::ExternalPath {
                component: ComponentId::Chronos,
                instrument_key: held_key.clone(),
                origin: session_t2,
                horizon: 21,
                path: PathQuantiles {
                    steps: vec![5, 10, 15, 20, 21],
                    values: vec![[-0.08, -0.05, -0.02, 0.01, 0.02]; 5],
                },
            }],
            t2,
        )
        .unwrap();

    // Replay T again: forecast values AND input_hash must be bit-identical!
    let outlook_1_again = svc
        .outlook_as_of(
            &account,
            &held_symbol,
            21,
            decision_t,
            &WeekdayCalendar,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();
    let forecast_1_again = svc
        .forecasts_as_of(
            &account,
            20,
            ModelId::TrailingDrift,
            decision_t,
            &WeekdayCalendar,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();
    let signals_1_again = svc
        .signals_as_of(&account, decision_t, VintagePolicy::ObservedOnly)
        .unwrap();

    assert_eq!(outlook_1.value, outlook_1_again.value);
    assert_eq!(
        outlook_1.manifest.input_hash,
        outlook_1_again.manifest.input_hash
    );
    assert_eq!(forecast_1.value, forecast_1_again.value);
    assert_eq!(
        forecast_1.manifest.input_hash,
        forecast_1_again.manifest.input_hash
    );
    assert_eq!(signals_1.value, signals_1_again.value);
    assert_eq!(
        signals_1.manifest.input_hash,
        signals_1_again.manifest.input_hash
    );

    // Replay at T2: later price correction, membership, corporate actions & v2 model ARE visible!
    let decision_t2 = DecisionTime::new(t2, session_t2);
    let outlook_2 = svc
        .outlook_as_of(
            &account,
            &held_symbol,
            21,
            decision_t2,
            &WeekdayCalendar,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();
    assert_ne!(outlook_1.manifest.input_hash, outlook_2.manifest.input_hash);
    assert_eq!(outlook_2.value.outlook.origin, session_t2);
    assert!(
        outlook_2
            .manifest
            .model_outputs
            .iter()
            .any(|m| m.model_version.as_deref() == Some("chronos-v2"))
    );
    assert!(
        outlook_1_again
            .manifest
            .model_outputs
            .iter()
            .all(|m| m.model_version.as_deref() != Some("chronos-v2"))
    );
}

#[test]
fn provenance_distinguishes_observed_from_reconstructed_and_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, svc, account) = make_service(tmp.path());

    let projection = svc
        .portfolio(
            &account,
            sail_capital::AsOf {
                occurred_by: ts(2026, 4, 1, 12, 0, 0),
                known_by: ts(2026, 4, 1, 12, 0, 0),
            },
        )
        .unwrap();
    let pos = projection.positions.first().unwrap();
    let held_key = pos.instrument.key();
    let held_symbol = pos.instrument.symbol.clone();

    // All bars ingested at T_archive, AFTER decision time T!
    let bars = synthetic_bars(d(2025, 1, 2), 360, 10_000);
    let session_t = bars[345].session;
    let t =
        DateTime::<Utc>::from_naive_utc_and_offset(session_t.and_hms_opt(22, 0, 0).unwrap(), Utc);
    let t_archive = t + Duration::days(30);

    store
        .record_prices(
            &held_key,
            &PriceSeries {
                source_id: "archive".into(),
                symbol: held_symbol.clone(),
                source_symbol: held_symbol.clone(),
                currency: Some(Currency::parse("USD").unwrap()),
                adjustment: Adjustment::SplitAdjusted,
                bars,
                payload_sha256: "archive-1".into(),
            },
            t_archive,
        )
        .unwrap();
    store
        .record_research_members(
            "sp500",
            &[ResearchMember {
                yahoo: "AAPL".into(),
                name: "Apple".into(),
                class: AssetClass::Stock,
                membership: vec![(d(2020, 1, 1), None)],
            }],
            t_archive,
        )
        .unwrap();

    let decision = DecisionTime::new(t, session_t);

    // Strict ObservedOnly sees no bars at T.
    let strict = svc
        .forecasts_as_of(
            &account,
            20,
            ModelId::NoChange,
            decision,
            &WeekdayCalendar,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();
    assert!(strict.value.forecasts.is_empty());
    assert_eq!(
        strict
            .manifest
            .bars
            .iter()
            .find(|b| b.instrument_key == held_key)
            .unwrap()
            .provenance,
        InputProvenance::Unavailable
    );

    // AllowReconstructed rebuilds from the later-stamped archive and labels it Reconstructed,
    // NEVER Observed!
    let recon = svc
        .forecasts_as_of(
            &account,
            20,
            ModelId::NoChange,
            decision,
            &WeekdayCalendar,
            VintagePolicy::AllowReconstructed,
        )
        .unwrap();
    assert!(!recon.value.forecasts.is_empty());
    assert_eq!(recon.manifest.provenance, InputProvenance::Reconstructed);
    assert_eq!(
        recon
            .manifest
            .bars
            .iter()
            .find(|b| b.instrument_key == held_key)
            .unwrap()
            .provenance,
        InputProvenance::Reconstructed
    );
    assert_eq!(
        recon
            .manifest
            .membership
            .iter()
            .find(|m| m.source == "research:sp500")
            .unwrap()
            .provenance,
        InputProvenance::Reconstructed
    );
}

#[test]
fn holiday_calendar_prevents_session_and_origin_drift() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, svc, account) = make_service(tmp.path());

    let projection = svc
        .portfolio(
            &account,
            sail_capital::AsOf {
                occurred_by: ts(2026, 4, 1, 12, 0, 0),
                known_by: ts(2026, 4, 1, 12, 0, 0),
            },
        )
        .unwrap();
    let pos = projection.positions.first().unwrap();
    let held_key = pos.instrument.key();
    let held_symbol = pos.instrument.symbol.clone();

    let bars = synthetic_bars(d(2025, 1, 2), 360, 10_000);
    // Pick a Friday in the series and mark it as an exchange holiday in a synthetic calendar.
    let holiday = bars
        .iter()
        .rev()
        .find(|b| b.session.weekday() == chrono::Weekday::Fri)
        .unwrap()
        .session;
    let prior_session = bars
        .iter()
        .rev()
        .find(|b| b.session < holiday)
        .unwrap()
        .session;

    let cal = HolidayCalendar::new("xnys-synth", [holiday]);
    assert!(!cal.is_session(holiday));
    assert_eq!(cal.session_on_or_before(holiday), Some(prior_session));

    let t = DateTime::<Utc>::from_naive_utc_and_offset(holiday.and_hms_opt(22, 0, 0).unwrap(), Utc);
    store
        .record_prices(
            &held_key,
            &PriceSeries {
                source_id: "synth".into(),
                symbol: held_symbol.clone(),
                source_symbol: held_symbol.clone(),
                currency: Some(Currency::parse("USD").unwrap()),
                adjustment: Adjustment::SplitAdjusted,
                bars,
                payload_sha256: "h1".into(),
            },
            t,
        )
        .unwrap();

    let decision = DecisionTime::new(t, holiday);
    let out = svc
        .outlook_as_of(
            &account,
            &held_symbol,
            21,
            decision,
            &cal,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();
    assert_eq!(out.value.outlook.origin, prior_session);
    assert_eq!(out.manifest.calendar_id, "xnys-synth");
}

#[test]
fn manifest_hash_is_deterministic_across_db_rewrites() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let (store_a, svc_a, acc_a) = make_service(dir_a.path());
    let (store_b, svc_b, acc_b) = make_service(dir_b.path());

    let proj = svc_a
        .portfolio(
            &acc_a,
            sail_capital::AsOf {
                occurred_by: ts(2026, 4, 1, 12, 0, 0),
                known_by: ts(2026, 4, 1, 12, 0, 0),
            },
        )
        .unwrap();
    let pos = proj.positions.first().unwrap();
    let key = pos.instrument.key();
    let sym = pos.instrument.symbol.clone();

    let bars = synthetic_bars(d(2025, 1, 2), 360, 10_000);
    let session_t = bars.last().unwrap().session;
    let t =
        DateTime::<Utc>::from_naive_utc_and_offset(session_t.and_hms_opt(22, 0, 0).unwrap(), Utc);

    // Insert an extra unrelated row in store_b first so all SQLite rowids differ!
    store_b
        .record_watch_change(
            &WatchChange::Remove {
                symbol: "NONEXISTENT".into(),
            },
            t - Duration::days(100),
        )
        .unwrap();

    for store in [&store_a, &store_b] {
        store
            .record_prices(
                &key,
                &PriceSeries {
                    source_id: "synth".into(),
                    symbol: sym.clone(),
                    source_symbol: sym.clone(),
                    currency: Some(Currency::parse("USD").unwrap()),
                    adjustment: Adjustment::SplitAdjusted,
                    bars: bars.clone(),
                    payload_sha256: "same".into(),
                },
                t,
            )
            .unwrap();
    }

    let decision = DecisionTime::new(t, session_t);
    let out_a = svc_a
        .forecasts_as_of(
            &acc_a,
            20,
            ModelId::EwmaVolatility,
            decision,
            &WeekdayCalendar,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();
    let out_b = svc_b
        .forecasts_as_of(
            &acc_b,
            20,
            ModelId::EwmaVolatility,
            decision,
            &WeekdayCalendar,
            VintagePolicy::ObservedOnly,
        )
        .unwrap();

    assert_eq!(out_a.manifest.input_hash, out_b.manifest.input_hash);
    assert_eq!(out_a.value.forecasts, out_b.value.forecasts);
}

#[test]
fn bar_input_hash_does_not_include_the_sqlite_row_id() {
    let session = d(2025, 1, 2);
    let at = ts(2025, 1, 2, 22, 0, 0);
    let observation = sail_capital::market::MarketObservation {
        instrument_key: "sym:AAPL.US".into(),
        symbol: "AAPL.US".into(),
        session,
        close: Decimal::new(10_000, 2),
        ohl: None,
        currency: Some(Currency::parse("USD").unwrap()),
        price_kind: "close".into(),
        adjustment: Adjustment::SplitAdjusted,
        source_id: "synth".into(),
        first_available_at: at,
        ingested_at: at,
    };
    let first = sail_capital::replay::hash_bar_revisions(&[(
        "1".into(),
        "bar-hash".into(),
        observation.clone(),
        InputProvenance::Observed,
    )]);
    let rewritten = sail_capital::replay::hash_bar_revisions(&[(
        "999".into(),
        "bar-hash".into(),
        observation,
        InputProvenance::Observed,
    )]);
    assert_eq!(first, rewritten);
}
