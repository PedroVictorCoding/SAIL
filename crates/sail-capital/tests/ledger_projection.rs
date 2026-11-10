//! End-to-end tests of `ledger::project` against hand-built records: the EPIC's miniature
//! portfolio, point-in-time visibility, determinism and reconciliation.
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use sail_capital::contracts::*;
use sail_capital::ledger::*;
use std::str::FromStr;

fn d(text: &str) -> Decimal {
    Decimal::from_str(text).expect("test decimal")
}

fn account() -> AccountId {
    AccountId::new(Broker::Xtb, "test")
}

fn usd() -> Currency {
    Currency::parse("USD").expect("USD")
}

fn eur() -> Currency {
    Currency::parse("EUR").expect("EUR")
}

fn money(amount: &str, currency: Currency) -> Money {
    Money::new(d(amount), currency)
}

fn time(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, day, hour, 0, 0)
        .single()
        .expect("test time")
}

fn bounds() -> AsOf {
    AsOf {
        occurred_by: Utc
            .with_ymd_and_hms(2026, 3, 1, 0, 0, 0)
            .single()
            .expect("test time"),
        known_by: Utc
            .with_ymd_and_hms(2026, 3, 1, 0, 0, 0)
            .single()
            .expect("test time"),
    }
}

fn row_ref(kind: SheetKind, row: u32) -> RowRef {
    RowRef {
        sheet: None,
        sheet_kind: kind,
        row,
    }
}

fn stock(symbol: &str) -> Instrument {
    Instrument {
        symbol: symbol.to_string(),
        isin: None,
        name: None,
        currency: Some(usd()),
        kind: InstrumentKind::Stock,
    }
}

fn record(
    id: &str,
    batch: &str,
    ingested: DateTime<Utc>,
    at: DateTime<Utc>,
    kind: EntryKind,
    cash: Option<Money>,
) -> LedgerRecord {
    LedgerRecord {
        id: id.to_string(),
        account: account(),
        batch_id: batch.to_string(),
        ingested_at: ingested,
        entry: ParsedEntry {
            natural_key: format!("xtb:cash:{id}"),
            source: row_ref(SheetKind::CashOperations, 1),
            broker_ref: Some(id.to_string()),
            occurred: EventTime {
                at,
                precision: TimePrecision::Exact,
            },
            kind,
            cash_effect: cash,
            comment: None,
        },
    }
}

/// The EPIC's definition of done in miniature: EUR and USD, two buys, a partial sell that
/// takes the oldest lot, a dividend with withholding, a commission, a deposit and a CFD
/// result.
fn miniature() -> Vec<LedgerRecord> {
    let ingested = time(31, 12);
    vec![
        record(
            "deposit",
            "batch-1",
            ingested,
            time(2, 10),
            EntryKind::Deposit,
            Some(money("5000", eur())),
        ),
        record(
            "buy-1",
            "batch-1",
            ingested,
            time(5, 10),
            EntryKind::Trade {
                instrument: stock("AAPL.US"),
                side: Side::Buy,
                quantity: Some(d("10")),
                price: Some(money("100", usd())),
                position_ref: None,
            },
            Some(money("-1000", usd())),
        ),
        record(
            "buy-2",
            "batch-1",
            ingested,
            time(6, 10),
            EntryKind::Trade {
                instrument: stock("AAPL.US"),
                side: Side::Buy,
                quantity: Some(d("10")),
                price: Some(money("120", usd())),
                position_ref: None,
            },
            Some(money("-1200", usd())),
        ),
        record(
            "dividend",
            "batch-1",
            ingested,
            time(10, 10),
            EntryKind::Dividend {
                instrument: Some(stock("AAPL.US")),
            },
            Some(money("20", usd())),
        ),
        record(
            "withholding",
            "batch-1",
            ingested,
            time(11, 10),
            EntryKind::Tax {
                kind: TaxKind::Withholding,
                instrument: Some(stock("AAPL.US")),
            },
            Some(money("-3", usd())),
        ),
        record(
            "sell-1",
            "batch-1",
            ingested,
            time(12, 10),
            EntryKind::Trade {
                instrument: stock("AAPL.US"),
                side: Side::Sell,
                quantity: Some(d("15")),
                price: Some(money("150", usd())),
                position_ref: None,
            },
            Some(money("2250", usd())),
        ),
        record(
            "commission",
            "batch-1",
            ingested,
            time(12, 11),
            EntryKind::Fee {
                kind: FeeKind::Commission,
                instrument: Some(stock("AAPL.US")),
            },
            Some(money("-2", usd())),
        ),
        record(
            "cfd-result",
            "batch-1",
            ingested,
            time(13, 10),
            EntryKind::DerivativeResult {
                instrument: Instrument {
                    symbol: "US500".to_string(),
                    isin: None,
                    name: None,
                    currency: Some(usd()),
                    kind: InstrumentKind::Cfd,
                },
                position_ref: None,
            },
            Some(money("50", eur())),
        ),
    ]
}

#[test]
fn miniature_portfolio_matches_hand_computed_totals() {
    let projection = project(&account(), &miniature(), &[], bounds());

    assert_eq!(projection.records_used, 8);
    assert_eq!(
        projection.cash,
        vec![money("5050", eur()), money("65", usd())]
    );

    assert_eq!(projection.positions.len(), 1);
    let position = &projection.positions[0];
    assert_eq!(position.instrument.key(), "sym:AAPL.US");
    assert_eq!(position.quantity, d("5"));
    assert_eq!(position.cost_basis, Some(money("600", usd())));
    assert_eq!(position.lots.len(), 1);
    assert_eq!(position.lots[0].quantity, d("5"));
    assert_eq!(position.lots[0].unit_cost, Some(money("120", usd())));

    assert!(projection.cfd_positions.is_empty());
    assert_eq!(projection.realized.len(), 1);
    let realized = &projection.realized[0];
    assert_eq!(realized.quantity, d("15"));
    assert_eq!(realized.proceeds, Some(money("2250", usd())));
    assert_eq!(realized.cost, Some(money("1600", usd())));
    assert_eq!(realized.result, Some(money("650", usd())));

    assert_eq!(projection.dividends.len(), 1);
    let dividend = &projection.dividends[0];
    assert_eq!(dividend.gross, money("20", usd()));
    assert_eq!(dividend.withheld, Some(money("3", usd())));
    assert_eq!(dividend.net, Some(money("17", usd())));
    assert_eq!(
        dividend.record_ids,
        vec!["dividend".to_string(), "withholding".to_string()]
    );

    let flows: Vec<(&str, &Money, u32)> = projection
        .flows
        .iter()
        .map(|flow| (flow.label.as_str(), &flow.total, flow.count))
        .collect();
    assert_eq!(
        flows,
        vec![
            ("deposit", &money("5000", eur()), 1),
            ("derivative_result", &money("50", eur()), 1),
            ("dividend", &money("20", usd()), 1),
            ("fee:commission", &money("-2", usd()), 1),
            ("trade:buy", &money("-2200", usd()), 2),
            ("trade:sell", &money("2250", usd()), 1),
            ("withholding", &money("-3", usd()), 1),
        ]
    );

    assert_eq!(
        projection.reconciliation.status,
        ReconciliationStatus::Unverified
    );
    assert_eq!(projection.reconciliation.compared, 0);
    assert!(projection.reconciliation.variances.is_empty());
    assert!(projection.warnings.is_empty(), "{:?}", projection.warnings);
}

#[test]
fn a_shuffled_input_gives_the_same_projection() {
    let mut shuffled = miniature();
    shuffled.reverse();
    shuffled.swap(0, 3);
    shuffled.swap(1, 5);

    let ordered = project(&account(), &miniature(), &[], bounds());
    let messy = project(&account(), &shuffled, &[], bounds());

    assert_eq!(ordered, messy);
}

#[test]
fn known_by_hides_a_later_import() {
    let first = record(
        "buy-1",
        "batch-1",
        time(20, 10),
        time(5, 10),
        EntryKind::Trade {
            instrument: stock("AAPL.US"),
            side: Side::Buy,
            quantity: Some(d("10")),
            price: Some(money("100", usd())),
            position_ref: None,
        },
        Some(money("-1000", usd())),
    );
    let second = record(
        "buy-2",
        "batch-2",
        time(31, 10),
        time(6, 10),
        EntryKind::Trade {
            instrument: stock("AAPL.US"),
            side: Side::Buy,
            quantity: Some(d("10")),
            price: Some(money("120", usd())),
            position_ref: None,
        },
        Some(money("-1200", usd())),
    );
    let as_of = AsOf {
        occurred_by: Utc
            .with_ymd_and_hms(2026, 3, 1, 0, 0, 0)
            .single()
            .expect("test time"),
        known_by: Utc
            .with_ymd_and_hms(2026, 1, 25, 0, 0, 0)
            .single()
            .expect("test time"),
    };

    let projection = project(&account(), &[second, first], &[], as_of);

    assert_eq!(projection.records_used, 1);
    assert_eq!(projection.cash, vec![money("-1000", usd())]);
    assert_eq!(projection.positions[0].quantity, d("10"));
}

#[test]
fn occurred_by_cuts_the_history_at_an_instant() {
    let buy = record(
        "buy",
        "batch-1",
        time(15, 10),
        time(5, 10),
        EntryKind::Trade {
            instrument: stock("AAPL.US"),
            side: Side::Buy,
            quantity: Some(d("10")),
            price: Some(money("100", usd())),
            position_ref: None,
        },
        Some(money("-1000", usd())),
    );
    let sell = record(
        "sell",
        "batch-1",
        time(15, 10),
        time(20, 10),
        EntryKind::Trade {
            instrument: stock("AAPL.US"),
            side: Side::Sell,
            quantity: Some(d("4")),
            price: Some(money("120", usd())),
            position_ref: None,
        },
        Some(money("480", usd())),
    );
    let as_of = AsOf {
        occurred_by: Utc
            .with_ymd_and_hms(2026, 1, 10, 0, 0, 0)
            .single()
            .expect("test time"),
        known_by: Utc
            .with_ymd_and_hms(2026, 1, 25, 0, 0, 0)
            .single()
            .expect("test time"),
    };

    let projection = project(&account(), &[sell, buy], &[], as_of);

    assert_eq!(projection.records_used, 1);
    assert_eq!(projection.cash, vec![money("-1000", usd())]);
    assert_eq!(projection.positions[0].quantity, d("10"));
    assert!(projection.realized.is_empty());
}

fn matched_ledger() -> Vec<LedgerRecord> {
    let ingested = time(31, 12);
    vec![
        record(
            "deposit",
            "batch-1",
            ingested,
            time(2, 10),
            EntryKind::Deposit,
            Some(money("500", usd())),
        ),
        record(
            "buy",
            "batch-1",
            ingested,
            time(5, 10),
            EntryKind::Trade {
                instrument: stock("AAPL.US"),
                side: Side::Buy,
                quantity: Some(d("5")),
                price: Some(money("100", usd())),
                position_ref: None,
            },
            Some(money("-500", usd())),
        ),
    ]
}

fn snapshot(quantity: &str, balance: &str) -> StoredSnapshot {
    StoredSnapshot {
        account: account(),
        batch_id: "batch-2".to_string(),
        ingested_at: time(31, 13),
        snapshot: BrokerSnapshot {
            as_of: Some(time(31, 10)),
            open_positions: vec![BrokerPosition {
                instrument: stock("AAPL.US"),
                position_ref: Some("123".to_string()),
                quantity: d(quantity),
                open_price: Some(money("100", usd())),
                opened: Some(EventTime {
                    at: time(5, 10),
                    precision: TimePrecision::Exact,
                }),
                market_price: Some(money("120", usd())),
                market_value: Some(money("600", usd())),
                unrealized: Some(money("100", usd())),
                source: row_ref(SheetKind::OpenPositions, 1),
            }],
            closed_positions: Vec::new(),
            balance: Some(money(balance, usd())),
            equity: Some(money("600", usd())),
        },
    }
}

#[test]
fn reconciliation_matches_when_every_figure_is_equal() {
    let projection = project(
        &account(),
        &matched_ledger(),
        &[snapshot("5", "0")],
        bounds(),
    );

    assert_eq!(projection.reconciliation.compared, 2);
    assert!(projection.reconciliation.variances.is_empty());
    assert_eq!(
        projection.reconciliation.status,
        ReconciliationStatus::Matched
    );
    assert_eq!(projection.snapshot_as_of, Some(time(31, 10)));
    assert_eq!(
        projection.positions[0].broker_market_value,
        Some(money("600", usd()))
    );
}

#[test]
fn reconciliation_reports_quantity_and_cash_variances() {
    let projection = project(
        &account(),
        &matched_ledger(),
        &[snapshot("6", "12.5")],
        bounds(),
    );

    assert_eq!(projection.reconciliation.compared, 2);
    assert_eq!(
        projection.reconciliation.status,
        ReconciliationStatus::Variance
    );
    let subjects: Vec<&str> = projection
        .reconciliation
        .variances
        .iter()
        .map(|variance| variance.subject.as_str())
        .collect();
    assert_eq!(subjects, vec!["quantity sym:AAPL.US", "cash USD"]);
    assert_eq!(
        projection.reconciliation.variances[0].ledger.as_deref(),
        Some("5")
    );
    assert_eq!(
        projection.reconciliation.variances[0].broker.as_deref(),
        Some("6")
    );
    assert_eq!(
        projection.reconciliation.variances[1].ledger.as_deref(),
        Some("0")
    );
    assert_eq!(
        projection.reconciliation.variances[1].broker.as_deref(),
        Some("12.5")
    );
}

#[test]
fn a_snapshot_from_the_future_is_ignored() {
    let as_of = AsOf {
        occurred_by: Utc
            .with_ymd_and_hms(2026, 1, 15, 0, 0, 0)
            .single()
            .expect("test time"),
        known_by: Utc
            .with_ymd_and_hms(2026, 3, 1, 0, 0, 0)
            .single()
            .expect("test time"),
    };

    let projection = project(&account(), &matched_ledger(), &[snapshot("5", "0")], as_of);

    assert_eq!(
        projection.reconciliation.status,
        ReconciliationStatus::Unverified
    );
    assert_eq!(projection.snapshot_as_of, None);
    assert_eq!(projection.positions[0].broker_market_value, None);
}
