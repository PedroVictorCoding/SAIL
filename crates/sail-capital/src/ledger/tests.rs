//! Unit tests for the ledger projection: ordering, cost derivation, FIFO edge cases, flow
//! labels and dividend matching. Every expected figure is a hand-written `Decimal`.
use super::*;
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
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

fn time(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, day, 10, 0, 0)
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

fn row_ref() -> RowRef {
    RowRef {
        sheet: None,
        sheet_kind: SheetKind::CashOperations,
        row: 1,
    }
}

fn stock(symbol: &str, currency: Currency) -> Instrument {
    Instrument {
        symbol: symbol.to_string(),
        isin: None,
        name: None,
        currency: Some(currency),
        kind: InstrumentKind::Stock,
    }
}

fn record(
    id: &str,
    occurred: DateTime<Utc>,
    precision: TimePrecision,
    kind: EntryKind,
    cash: Option<Money>,
) -> LedgerRecord {
    LedgerRecord {
        id: id.to_string(),
        account: account(),
        batch_id: "batch-1".to_string(),
        ingested_at: time(28),
        entry: ParsedEntry {
            natural_key: format!("xtb:cash:{id}"),
            source: row_ref(),
            broker_ref: Some(id.to_string()),
            occurred: EventTime {
                at: occurred,
                precision,
            },
            kind,
            cash_effect: cash,
            comment: None,
        },
    }
}

fn trade(
    id: &str,
    day: u32,
    side: Side,
    quantity: Option<&str>,
    price: Option<Money>,
    cash: Option<Money>,
) -> LedgerRecord {
    record(
        id,
        time(day),
        TimePrecision::Exact,
        EntryKind::Trade {
            instrument: stock("AAPL.US", usd()),
            side,
            quantity: quantity.map(d),
            price,
            position_ref: None,
        },
        cash,
    )
}

#[test]
fn date_only_row_sorts_before_a_timed_row_on_the_same_instant() {
    let instant = time(5);
    let exact_buy = record(
        "aaa-exact",
        instant,
        TimePrecision::Exact,
        EntryKind::Trade {
            instrument: stock("AAPL.US", usd()),
            side: Side::Buy,
            quantity: Some(d("1")),
            price: Some(money("100", usd())),
            position_ref: None,
        },
        Some(money("-100", usd())),
    );
    let date_buy = record(
        "zzz-date",
        instant,
        TimePrecision::DateOnly,
        EntryKind::Trade {
            instrument: stock("AAPL.US", usd()),
            side: Side::Buy,
            quantity: Some(d("1")),
            price: Some(money("200", usd())),
            position_ref: None,
        },
        Some(money("-200", usd())),
    );
    let sell = trade(
        "sell",
        6,
        Side::Sell,
        Some("1"),
        None,
        Some(money("250", usd())),
    );

    let projection = project(&account(), &[exact_buy, date_buy, sell], &[], bounds());

    let realized = &projection.realized[0];
    assert_eq!(realized.quantity, d("1"));
    assert_eq!(realized.cost, Some(money("200", usd())));
    assert_eq!(realized.result, Some(money("50", usd())));
}

#[test]
fn unit_cost_falls_back_to_cash_only_when_currencies_match() {
    let matching = trade(
        "buy-match",
        5,
        Side::Buy,
        Some("10"),
        None,
        Some(money("-100", usd())),
    );
    let projection = project(&account(), &[matching], &[], bounds());
    let lot = &projection.positions[0].lots[0];
    assert_eq!(lot.unit_cost, Some(money("10", usd())));

    let mismatched = record(
        "buy-mismatch",
        time(6),
        TimePrecision::Exact,
        EntryKind::Trade {
            instrument: stock("SAP.DE", eur()),
            side: Side::Buy,
            quantity: Some(d("2")),
            price: None,
            position_ref: None,
        },
        Some(money("-200", usd())),
    );
    let projection = project(&account(), &[mismatched], &[], bounds());
    assert_eq!(projection.positions[0].lots[0].unit_cost, None);
}

#[test]
fn an_oversell_warns_and_never_goes_negative() {
    let buy = trade(
        "buy",
        5,
        Side::Buy,
        Some("5"),
        Some(money("100", usd())),
        Some(money("-500", usd())),
    );
    let sell = trade(
        "sell",
        6,
        Side::Sell,
        Some("10"),
        None,
        Some(money("900", usd())),
    );

    let projection = project(&account(), &[buy, sell], &[], bounds());

    assert!(projection.positions.is_empty());
    assert!(projection.cfd_positions.is_empty());
    assert_eq!(projection.realized.len(), 1);
    assert_eq!(projection.realized[0].quantity, d("5"));
    assert!(
        projection
            .warnings
            .iter()
            .any(|warning| warning.contains("found only 5")),
        "warnings: {:?}",
        projection.warnings
    );
    assert!(
        projection
            .reconciliation
            .variances
            .iter()
            .any(|variance| variance.subject == "fifo sym:AAPL.US"),
        "variances: {:?}",
        projection.reconciliation.variances
    );
    assert_eq!(
        projection.reconciliation.status,
        ReconciliationStatus::Unverified
    );
}

#[test]
fn withholding_matches_the_nearest_dividend_only_once() {
    let dividend_one = record(
        "div-1",
        time(10),
        TimePrecision::Exact,
        EntryKind::Dividend {
            instrument: Some(stock("AAPL.US", usd())),
        },
        Some(money("20", usd())),
    );
    let dividend_two = record(
        "div-2",
        time(20),
        TimePrecision::Exact,
        EntryKind::Dividend {
            instrument: Some(stock("AAPL.US", usd())),
        },
        Some(money("30", usd())),
    );
    let nearest = record(
        "tax-nearest",
        time(11),
        TimePrecision::Exact,
        EntryKind::Tax {
            kind: TaxKind::Withholding,
            instrument: Some(stock("AAPL.US", usd())),
        },
        Some(money("-3", usd())),
    );
    let farther = record(
        "tax-farther",
        time(12),
        TimePrecision::Exact,
        EntryKind::Tax {
            kind: TaxKind::Withholding,
            instrument: Some(stock("AAPL.US", usd())),
        },
        Some(money("-1", usd())),
    );
    let second = record(
        "tax-second",
        time(21),
        TimePrecision::Exact,
        EntryKind::Tax {
            kind: TaxKind::Withholding,
            instrument: Some(stock("AAPL.US", usd())),
        },
        Some(money("-4", usd())),
    );

    let projection = project(
        &account(),
        &[farther, second, dividend_two, nearest, dividend_one],
        &[],
        bounds(),
    );

    assert_eq!(projection.dividends.len(), 2);
    assert_eq!(projection.dividends[0].withheld, Some(money("3", usd())));
    assert_eq!(projection.dividends[0].net, Some(money("17", usd())));
    assert_eq!(projection.dividends[1].withheld, Some(money("4", usd())));
    assert_eq!(projection.dividends[1].net, Some(money("26", usd())));
    assert!(
        projection
            .warnings
            .iter()
            .any(|warning| warning.contains("tax-farther")),
        "warnings: {:?}",
        projection.warnings
    );
    assert_eq!(projection.cash[0].amount, d("42"));
}

#[test]
fn an_unmatched_withholding_warns_and_stays_in_the_flows() {
    let withholding = record(
        "tax",
        time(11),
        TimePrecision::Exact,
        EntryKind::Tax {
            kind: TaxKind::Withholding,
            instrument: Some(stock("AAPL.US", usd())),
        },
        Some(money("-3", usd())),
    );

    let projection = project(&account(), &[withholding], &[], bounds());

    assert!(projection.dividends.is_empty());
    assert_eq!(projection.cash.len(), 1);
    assert_eq!(projection.cash[0], money("-3", usd()));
    assert_eq!(projection.flows[0].label, "withholding");
    assert_eq!(projection.flows[0].total, money("-3", usd()));
    assert!(
        projection
            .warnings
            .iter()
            .any(|warning| warning.contains("no matching dividend")),
        "warnings: {:?}",
        projection.warnings
    );
}

#[test]
fn cash_and_flows_are_per_currency_and_keep_unsupported_rows() {
    let deposit = record(
        "deposit",
        time(2),
        TimePrecision::Exact,
        EntryKind::Deposit,
        Some(money("100", eur())),
    );
    let unsupported = record(
        "unsupported",
        time(3),
        TimePrecision::Exact,
        EntryKind::Unsupported {
            raw_type: "Mystery".to_string(),
        },
        Some(money("-7.5", usd())),
    );

    let projection = project(&account(), &[deposit, unsupported], &[], bounds());

    assert_eq!(
        projection.cash,
        vec![money("100", eur()), money("-7.5", usd())]
    );
    let labels: Vec<&str> = projection
        .flows
        .iter()
        .map(|flow| flow.label.as_str())
        .collect();
    assert_eq!(labels, vec!["deposit", "unsupported"]);
    assert_eq!(projection.flows[1].count, 1);
}

#[test]
fn a_trade_without_a_quantity_warns_once_at_changes_only_cash() {
    let first = trade(
        "buy-1",
        5,
        Side::Buy,
        None,
        None,
        Some(money("-100", usd())),
    );
    let second = trade(
        "buy-2",
        6,
        Side::Buy,
        None,
        None,
        Some(money("-200", usd())),
    );

    let projection = project(&account(), &[second, first], &[], bounds());

    assert!(projection.positions.is_empty());
    let warnings: Vec<&String> = projection
        .warnings
        .iter()
        .filter(|warning| warning.contains("no stated quantity"))
        .collect();
    assert_eq!(warnings.len(), 1);
    assert_eq!(projection.cash[0], money("-300", usd()));
}

#[test]
fn mixed_currency_lots_leave_the_cost_basis_unknown() {
    let eur_buy = record(
        "buy-eur",
        time(5),
        TimePrecision::Exact,
        EntryKind::Trade {
            instrument: stock("AAPL.US", usd()),
            side: Side::Buy,
            quantity: Some(d("5")),
            price: Some(money("10", eur())),
            position_ref: None,
        },
        Some(money("-50", eur())),
    );
    let usd_buy = trade(
        "buy-usd",
        6,
        Side::Buy,
        Some("5"),
        Some(money("10", usd())),
        Some(money("-50", usd())),
    );

    let projection = project(&account(), &[eur_buy, usd_buy], &[], bounds());

    assert_eq!(projection.positions.len(), 1);
    assert_eq!(projection.positions[0].quantity, d("10"));
    assert_eq!(projection.positions[0].cost_basis, None);
}
