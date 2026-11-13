//! The board's forward record: snapshots, grading, summaries, storage and the service.
//! Everything here is synthetic; no network and no real export.
use chrono::{Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use sail_capital::board::{
    BoardMeta, BoardRow, Combined, Lean, OutlookSummary, Read, Reading, SignalBoard,
};
use sail_capital::board_record::{
    GRADE_HORIZONS, GradedRead, MIN_PEERS, RECENT_GRADED, ReadSnapshot, grade, snapshot, summarize,
};
use sail_capital::contracts::{AccountId, Broker};
use sail_capital::market::{Adjustment, PriceBar, PriceSeries};
use sail_capital::service::{CapitalService, watch_key};
use sail_capital::signals::SignalId;
use sail_capital::store::CapitalStore;
use sail_capital::trade::AssetClass;
use sail_capital::universe::{WatchChange, WatchItem};
use std::collections::BTreeMap;

const CASH: &[u8] = include_bytes!("fixtures/xtb/english_cash.csv");

fn day(offset: i64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 5).unwrap() + Duration::days(offset)
}

fn sessions(count: usize) -> Vec<NaiveDate> {
    (0..count).map(|index| day(index as i64 * 2)).collect()
}

fn read(lean: Lean, proven: bool) -> Read {
    Read {
        lean,
        proven,
        for_count: 0,
        against_count: 0,
        sentence: String::new(),
        short: String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn board_row(
    key: &str,
    symbol: &str,
    held: bool,
    session: Option<NaiveDate>,
    close: Option<f64>,
    lean: Lean,
    proven: bool,
    readings: Vec<Reading>,
    outlook: Vec<OutlookSummary>,
) -> BoardRow {
    BoardRow {
        meta: BoardMeta {
            key: key.to_string(),
            symbol: symbol.to_string(),
            name: symbol.to_string(),
            class: "stock".to_string(),
            held,
        },
        session,
        close,
        change_1m: None,
        readings,
        combined: Vec::<Combined>::new(),
        outlook,
        setup: None,
        read: read(lean, proven),
    }
}

fn reading(signal: SignalId, percentile: f64) -> Reading {
    Reading {
        signal,
        raw: 0.0,
        text: String::new(),
        percentile,
    }
}

fn outlook(horizon: u32, p_up: f64) -> OutlookSummary {
    OutlookSummary {
        horizon_sessions: horizon,
        median: 0.01,
        low: -0.10,
        high: 0.10,
        p_up,
    }
}

fn empty_board(rows: Vec<BoardRow>) -> SignalBoard {
    SignalBoard {
        session: rows.iter().filter_map(|row| row.session).max(),
        held: 0,
        watched: 0,
        passed: Vec::new(),
        combined_ic: Vec::new(),
        skill: Vec::new(),
        outlook_record: Vec::new(),
        setups_session: None,
        rows,
        notes: Vec::new(),
    }
}

fn snap(
    key: &str,
    symbol: &str,
    session: NaiveDate,
    close: f64,
    lean: Lean,
    outlook: Vec<OutlookSummary>,
) -> ReadSnapshot {
    ReadSnapshot {
        session,
        key: key.to_string(),
        symbol: symbol.to_string(),
        held: true,
        lean,
        proven: false,
        close,
        percentiles: Vec::new(),
        outlook,
    }
}

#[allow(clippy::too_many_arguments)]
fn graded(
    session: NaiveDate,
    symbol: &str,
    lean: Lean,
    proven: bool,
    horizon: u32,
    outcome: f64,
    board_return: f64,
    p_up: Option<f64>,
    median: Option<f64>,
    inside_90: Option<bool>,
) -> GradedRead {
    GradedRead {
        session,
        symbol: symbol.to_string(),
        held: true,
        lean,
        proven,
        horizon_sessions: horizon,
        outcome,
        board_return,
        excess: outcome - board_return,
        beat_board: outcome > board_return,
        p_up,
        median,
        inside_90,
    }
}

fn price_series(symbol: &str, start: NaiveDate, count: usize, base: i64) -> PriceSeries {
    PriceSeries {
        source_id: "test".into(),
        symbol: symbol.into(),
        source_symbol: symbol.to_lowercase(),
        currency: None,
        adjustment: Adjustment::Unknown,
        bars: (0..count)
            .map(|index| PriceBar {
                session: start + Duration::days(index as i64),
                open: None,
                high: None,
                low: None,
                close: Decimal::from(base + index as i64),
                volume: None,
            })
            .collect(),
        payload_sha256: "x".into(),
    }
}

#[test]
fn snapshot_skips_rows_without_a_close_and_keeps_the_rest() {
    let board = empty_board(vec![
        board_row(
            "sym:AAA.US",
            "AAA.US",
            true,
            Some(day(0)),
            Some(100.0),
            Lean::Favoured,
            true,
            vec![
                reading(SignalId::Momentum12_1, 80.0),
                reading(SignalId::Trend, 20.0),
            ],
            vec![outlook(21, 0.6)],
        ),
        board_row(
            "sym:BBB.US",
            "BBB.US",
            false,
            None,
            Some(50.0),
            Lean::Neutral,
            false,
            Vec::new(),
            Vec::new(),
        ),
        board_row(
            "sym:CCC.US",
            "CCC.US",
            false,
            Some(day(0)),
            None,
            Lean::Mixed,
            false,
            Vec::new(),
            Vec::new(),
        ),
        board_row(
            "sym:DDD.US",
            "DDD.US",
            false,
            Some(day(0)),
            Some(25.0),
            Lean::NoData,
            false,
            Vec::new(),
            Vec::new(),
        ),
    ]);

    let reads = snapshot(&board);

    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0].key, "sym:AAA.US");
    assert_eq!(reads[0].symbol, "AAA.US");
    assert!(reads[0].held);
    assert_eq!(reads[0].session, day(0));
    assert_eq!(reads[0].close, 100.0);
    assert_eq!(reads[0].lean, Lean::Favoured);
    assert!(reads[0].proven);
    assert_eq!(
        reads[0].percentiles,
        vec![(SignalId::Momentum12_1, 80.0), (SignalId::Trend, 20.0)]
    );
    assert_eq!(reads[0].outlook, vec![outlook(21, 0.6)]);
    assert_eq!(reads[1].lean, Lean::NoData);
}

#[test]
fn grade_uses_the_instruments_own_later_sessions() {
    let days = sessions(70);
    let start = days[0];
    let snapshots: Vec<ReadSnapshot> = (0..5)
        .map(|index| {
            snap(
                &format!("k{index}"),
                &format!("S{index}"),
                start,
                100.0,
                Lean::Favoured,
                vec![outlook(21, 0.6)],
            )
        })
        .collect();
    let mut closes: BTreeMap<String, Vec<(NaiveDate, f64)>> = BTreeMap::new();
    for index in 0..5 {
        closes.insert(
            format!("k{index}"),
            days.iter()
                .enumerate()
                .map(|(session, date)| {
                    (
                        *date,
                        100.0 * (1.0 + 0.01 * index as f64) * (1.0 + 0.001 * session as f64),
                    )
                })
                .collect(),
        );
    }
    let closer = |key: &str| closes.get(key).cloned().unwrap_or_default();

    let graded = grade(&snapshots, &closer);

    // Two horizons; the gapped calendar still counts 21 and 63 later sessions.
    assert_eq!(graded.len(), 10);
    let first = graded
        .iter()
        .find(|read| read.symbol == "S0" && read.horizon_sessions == 21)
        .unwrap();
    let expected = closes["k0"][21].1 / 100.0 - 1.0;
    assert!((first.outcome - expected).abs() < 1e-12);
    assert_eq!(first.horizon_sessions, 21);
    // The board return is the median peer outcome at that horizon (S2).
    let median = closes["k2"][21].1 / 100.0 - 1.0;
    assert!((first.board_return - median).abs() < 1e-12);
    assert!((first.excess - (expected - median)).abs() < 1e-12);
    assert!(!first.beat_board);
    // An outlook at 21 gives p_up and inside_90; none at 63.
    assert_eq!(first.p_up, Some(0.6));
    assert_eq!(first.inside_90, Some(true));
    let long = graded
        .iter()
        .find(|read| read.symbol == "S0" && read.horizon_sessions == 63)
        .unwrap();
    assert_eq!(long.p_up, None);
    assert_eq!(long.median, None);
    assert_eq!(long.inside_90, None);
    let best = graded
        .iter()
        .find(|read| read.symbol == "S4" && read.horizon_sessions == 21)
        .unwrap();
    assert!(best.beat_board);
}

#[test]
fn grade_waits_for_the_horizon_and_min_peers() {
    let start = day(0);

    let five = || -> Vec<ReadSnapshot> {
        (0..5)
            .map(|index| {
                snap(
                    &format!("k{index}"),
                    &format!("S{index}"),
                    start,
                    100.0,
                    Lean::Favoured,
                    Vec::new(),
                )
            })
            .collect()
    };
    let closes_with = |count: usize| {
        let mut map: BTreeMap<String, Vec<(NaiveDate, f64)>> = BTreeMap::new();
        for index in 0..5 {
            map.insert(
                format!("k{index}"),
                (0..count)
                    .map(|session| (day(session as i64), 100.0 + index as f64 + session as f64))
                    .collect(),
            );
        }
        map
    };

    // 20 later sessions: 21 sessions in all, so the 1-month horizon is not reached.
    let twenty_one = closes_with(21);
    let before = grade(&five(), &|key: &str| {
        twenty_one.get(key).cloned().unwrap_or_default()
    });
    assert!(before.is_empty());

    // 21 later sessions reach the 1-month horizon; 3 months is still ahead.
    let twenty_two = closes_with(22);
    let reached = grade(&five(), &|key: &str| {
        twenty_two.get(key).cloned().unwrap_or_default()
    });
    assert_eq!(reached.len(), 5);
    assert!(
        reached
            .iter()
            .all(|read| read.horizon_sessions == GRADE_HORIZONS[0])
    );

    // Four graded peers are one short of MIN_PEERS, even with enough sessions.
    let four = five();
    let enough = closes_with(70);
    let short = grade(&four[..MIN_PEERS - 1], &|key: &str| {
        enough.get(key).cloned().unwrap_or_default()
    });
    assert!(short.is_empty());

    // A NoData read never counts as a graded peer and is never graded itself.
    let mut six: Vec<ReadSnapshot> = (0..6)
        .map(|index| {
            snap(
                &format!("k{index}"),
                &format!("S{index}"),
                start,
                100.0,
                Lean::Favoured,
                Vec::new(),
            )
        })
        .collect();
    six[0].lean = Lean::NoData;
    let mut six_closes = enough.clone();
    for index in 0..6 {
        six_closes.insert(
            format!("k{index}"),
            (0..70)
                .map(|session| (day(session as i64), 100.0 + index as f64 + session as f64))
                .collect(),
        );
    }
    let no_data = grade(&six, &|key: &str| {
        six_closes.get(key).cloned().unwrap_or_default()
    });
    assert!(no_data.iter().all(|read| read.lean != Lean::NoData));
    assert_eq!(no_data.len(), 5 * GRADE_HORIZONS.len());
}

#[test]
fn summarize_orders_by_lean_proof_and_horizon() {
    let snapshots = vec![snap("k", "S", day(0), 100.0, Lean::Favoured, Vec::new())];
    let graded = vec![
        graded(
            day(0),
            "S",
            Lean::Disfavoured,
            false,
            21,
            0.1,
            0.0,
            None,
            None,
            None,
        ),
        graded(
            day(0),
            "S",
            Lean::Mixed,
            false,
            21,
            0.1,
            0.0,
            None,
            None,
            None,
        ),
        graded(
            day(0),
            "S",
            Lean::Neutral,
            false,
            21,
            0.1,
            0.0,
            None,
            None,
            None,
        ),
        graded(
            day(0),
            "S",
            Lean::Favoured,
            false,
            21,
            0.1,
            0.0,
            None,
            None,
            None,
        ),
        graded(
            day(0),
            "S",
            Lean::Favoured,
            false,
            63,
            0.1,
            0.0,
            None,
            None,
            None,
        ),
        graded(
            day(0),
            "S",
            Lean::Favoured,
            true,
            63,
            0.1,
            0.0,
            None,
            None,
            None,
        ),
        graded(
            day(0),
            "S",
            Lean::Favoured,
            true,
            21,
            0.1,
            0.0,
            None,
            None,
            None,
        ),
        graded(
            day(0),
            "S",
            Lean::NoData,
            false,
            21,
            0.1,
            0.0,
            None,
            None,
            None,
        ),
    ];

    let record = summarize(&snapshots, &graded);

    let order: Vec<(Lean, bool, u32)> = record
        .by_lean
        .iter()
        .map(|entry| (entry.lean, entry.proven, entry.horizon_sessions))
        .collect();
    assert_eq!(
        order,
        vec![
            (Lean::Favoured, true, 21),
            (Lean::Favoured, true, 63),
            (Lean::Favoured, false, 21),
            (Lean::Favoured, false, 63),
            (Lean::Neutral, false, 21),
            (Lean::Mixed, false, 21),
            (Lean::Disfavoured, false, 21),
        ]
    );
    assert!(
        !record
            .by_lean
            .iter()
            .any(|entry| entry.lean == Lean::NoData)
    );
}

#[test]
fn summarize_interval_handles_one_and_three_reads() {
    let snapshots = vec![snap("k", "A", day(0), 100.0, Lean::Favoured, Vec::new())];

    let one = summarize(
        &snapshots,
        &[graded(
            day(0),
            "A",
            Lean::Favoured,
            false,
            21,
            0.05,
            0.0,
            None,
            None,
            None,
        )],
    );
    assert_eq!(one.by_lean[0].reads, 1);
    assert_eq!(one.by_lean[0].mean_excess, 0.05);
    assert_eq!(one.by_lean[0].excess_ci, (0.05, 0.05));
    assert_eq!(one.by_lean[0].beat_board_rate, 1.0);

    let three = summarize(
        &snapshots,
        &[
            graded(
                day(0),
                "A",
                Lean::Favoured,
                false,
                21,
                0.0,
                0.0,
                None,
                None,
                None,
            ),
            graded(
                day(0),
                "B",
                Lean::Favoured,
                false,
                21,
                0.1,
                0.0,
                None,
                None,
                None,
            ),
            graded(
                day(0),
                "C",
                Lean::Favoured,
                false,
                21,
                0.2,
                0.0,
                None,
                None,
                None,
            ),
        ],
    );
    let entry = &three.by_lean[0];
    assert_eq!(entry.reads, 3);
    assert!((entry.mean_excess - 0.1).abs() < 1e-12);
    let margin = 1.96 * 0.1 / 3f64.sqrt();
    assert!((entry.excess_ci.0 - (0.1 - margin)).abs() < 1e-12);
    assert!((entry.excess_ci.1 - (0.1 + margin)).abs() < 1e-12);
    assert!((entry.beat_board_rate - 2.0 / 3.0).abs() < 1e-12);
}

#[test]
fn summarize_calibration_uses_hand_computed_brier_scores() {
    let snapshots = vec![snap("k", "A", day(0), 100.0, Lean::Favoured, Vec::new())];
    let graded = vec![
        graded(
            day(0),
            "A",
            Lean::Favoured,
            false,
            21,
            0.1,
            0.0,
            Some(0.8),
            Some(0.05),
            Some(true),
        ),
        graded(
            day(0),
            "B",
            Lean::Favoured,
            false,
            21,
            -0.1,
            0.0,
            Some(0.4),
            Some(-0.05),
            Some(false),
        ),
        graded(
            day(0),
            "C",
            Lean::Favoured,
            false,
            21,
            0.2,
            0.0,
            Some(0.6),
            Some(0.0),
            Some(true),
        ),
        graded(
            day(0),
            "D",
            Lean::Favoured,
            false,
            63,
            0.2,
            0.0,
            None,
            None,
            None,
        ),
    ];

    let record = summarize(&snapshots, &graded);

    assert_eq!(record.outlook.len(), 1);
    let calibration = &record.outlook[0];
    assert_eq!(calibration.horizon_sessions, 21);
    assert_eq!(calibration.graded, 3);
    assert!((calibration.inside_90 - 2.0 / 3.0).abs() < 1e-12);
    assert!((calibration.brier - 0.12).abs() < 1e-12);
    assert!((calibration.brier_base_rate - 2.0 / 9.0).abs() < 1e-12);
    // Two non-zero medians, both right; the zero median is skipped.
    assert!((calibration.direction_hit_rate - 1.0).abs() < 1e-12);
}

#[test]
fn recent_is_newest_first_then_symbol_and_capped() {
    let snapshots = vec![snap("k", "A", day(0), 100.0, Lean::Favoured, Vec::new())];
    let mut reads = Vec::new();
    for session in 0..20 {
        for symbol in ["B", "A", "C"] {
            reads.push(graded(
                day(session),
                symbol,
                Lean::Favoured,
                false,
                21,
                0.1,
                0.0,
                None,
                None,
                None,
            ));
        }
    }

    let record = summarize(&snapshots, &reads);

    assert_eq!(record.recent.len(), RECENT_GRADED);
    assert_eq!(record.recent[0].session, day(19));
    assert_eq!(
        record.recent[..3]
            .iter()
            .map(|read| read.symbol.as_str())
            .collect::<Vec<_>>(),
        vec!["A", "B", "C"]
    );
    assert!(
        record
            .recent
            .windows(2)
            .all(|pair| pair[0].session >= pair[1].session)
    );
}

#[test]
fn nothing_graded_yet_says_when_the_first_grades_arrive() {
    let snapshots = vec![snap("k", "A", day(0), 100.0, Lean::Favoured, Vec::new())];

    let record = summarize(&snapshots, &[]);

    assert_eq!(record.graded, 0);
    assert!(record.by_lean.is_empty());
    assert!(record.outlook.is_empty());
    assert!(record.recent.is_empty());
    assert!(record.notes[0].contains("first 1-month grades"));
    assert!(
        record
            .notes
            .iter()
            .any(|note| note.contains("share outcomes"))
    );
    assert!(
        record
            .notes
            .iter()
            .any(|note| note.contains("Recording since"))
    );
}

#[test]
fn store_keeps_the_first_write_and_orders_reads() {
    let dir = tempfile::tempdir().unwrap();
    let store = CapitalStore::open(dir.path()).unwrap();
    let account = AccountId::new(Broker::Xtb, "1234");
    let now = Utc::now();
    let first = snap("k2", "S2", day(0), 100.0, Lean::Favoured, Vec::new());
    let second = snap("k1", "S1", day(0), 50.0, Lean::Neutral, Vec::new());

    assert_eq!(
        store
            .record_board_reads(&account, &[first.clone(), second], now)
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .record_board_reads(&account, std::slice::from_ref(&first), now)
            .unwrap(),
        0
    );
    let mut changed = first.clone();
    changed.close = 999.0;
    assert_eq!(
        store.record_board_reads(&account, &[changed], now).unwrap(),
        0
    );

    let reads = store.board_reads(&account).unwrap();
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0].key, "k1");
    assert_eq!(reads[1].key, "k2");
    assert_eq!(reads[1].close, 100.0);
    assert_eq!(store.latest_board_session(&account).unwrap(), Some(day(0)));

    assert_eq!(
        store
            .record_board_reads(
                &account,
                &[snap("k1", "S1", day(1), 51.0, Lean::Neutral, Vec::new())],
                now,
            )
            .unwrap(),
        1
    );
    assert_eq!(store.latest_board_session(&account).unwrap(), Some(day(1)));
}

fn watch_item(symbol: &str) -> WatchItem {
    WatchItem {
        symbol: symbol.to_string(),
        yahoo: symbol.trim_end_matches(".US").to_string(),
        class: AssetClass::Stock,
        currency: "USD".to_string(),
        name: symbol.to_string(),
    }
}

#[test]
fn service_without_prices_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let service = CapitalService::open(dir.path()).unwrap();
    service
        .import_xtb(None, "cash.csv", CASH, None, None, Utc::now())
        .unwrap();
    let account = service.resolve_account(None).unwrap();

    assert_eq!(
        service.record_board_snapshot(&account, Utc::now()).unwrap(),
        0
    );
    let record = service.board_record(&account).unwrap();

    assert_eq!(record.graded, 0);
    assert!(record.by_lean.is_empty());
    assert!(record.outlook.is_empty());
    assert!(record.recent.is_empty());
    assert!(record.first_session.is_none());
    assert!(record.notes.iter().any(|note| note.contains("No sessions")));
}

#[test]
fn service_snapshots_once_per_session_and_grades_after_a_month() {
    let dir = tempfile::tempdir().unwrap();
    let start = day(0);
    let symbols = ["KILO1.US", "KILO2.US", "KILO3.US", "KILO4.US", "KILO5.US"];
    {
        let store = CapitalStore::open(dir.path()).unwrap();
        let now = Utc::now();
        for (index, symbol) in symbols.iter().enumerate() {
            store
                .record_watch_change(
                    &WatchChange::Add {
                        item: watch_item(symbol),
                    },
                    now,
                )
                .unwrap();
            store
                .record_prices(
                    &watch_key(symbol),
                    &price_series(symbol, start, 26, 100 + index as i64 * 10),
                    now,
                )
                .unwrap();
        }
    }

    let service = CapitalService::open(dir.path()).unwrap();
    service
        .import_xtb(None, "cash.csv", CASH, None, None, Utc::now())
        .unwrap();
    let account = service.resolve_account(None).unwrap();

    let first = service.record_board_snapshot(&account, Utc::now()).unwrap();
    assert_eq!(first, symbols.len() as u32);
    assert_eq!(
        service.record_board_snapshot(&account, Utc::now()).unwrap(),
        0
    );

    {
        let store = CapitalStore::open(dir.path()).unwrap();
        let now = Utc::now();
        for (index, symbol) in symbols.iter().enumerate() {
            store
                .record_prices(
                    &watch_key(symbol),
                    &price_series(symbol, start, 51, 100 + index as i64 * 10),
                    now,
                )
                .unwrap();
        }
    }

    let record = service.board_record(&account).unwrap();

    assert_eq!(record.graded, symbols.len() as u32);
    assert!(
        record
            .by_lean
            .iter()
            .any(|entry| entry.horizon_sessions == 21)
    );
    assert!(
        record
            .by_lean
            .iter()
            .all(|entry| entry.horizon_sessions == 21)
    );
    assert_eq!(record.recent.len(), symbols.len());
    assert_eq!(record.first_session, Some(start + Duration::days(25)));
    assert_eq!(record.sessions, 1);
}
