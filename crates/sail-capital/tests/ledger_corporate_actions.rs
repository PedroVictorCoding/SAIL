//! Synthetic tests for splits and owner grants in the ledger projection. Every figure is
//! hand-computed; no real export or account data is used.
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

fn stock(symbol: &str) -> Instrument {
    Instrument {
        symbol: symbol.to_string(),
        isin: None,
        name: None,
        currency: Some(usd()),
        kind: InstrumentKind::Stock,
    }
}

fn record(id: &str, at: DateTime<Utc>, kind: EntryKind, cash: Option<Money>) -> LedgerRecord {
    LedgerRecord {
        id: id.to_string(),
        account: account(),
        batch_id: "batch-1".to_string(),
        ingested_at: time(31, 12),
        entry: ParsedEntry {
            natural_key: format!("xtb:cash:{id}"),
            source: RowRef {
                sheet: None,
                sheet_kind: SheetKind::CashOperations,
                row: 1,
            },
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

fn trade(
    id: &str,
    day: u32,
    side: Side,
    quantity: &str,
    price: Option<Money>,
    cash: Option<Money>,
) -> LedgerRecord {
    record(
        id,
        time(day, 10),
        EntryKind::Trade {
            instrument: stock("LCID.US"),
            side,
            quantity: Some(d(quantity)),
            price,
            position_ref: None,
        },
        cash,
    )
}

fn split(
    id: &str,
    day: u32,
    new_units: &str,
    old_units: &str,
    cash: Option<Money>,
) -> LedgerRecord {
    record(
        id,
        time(day, 10),
        EntryKind::CorporateAction {
            instrument: stock("LCID.US"),
            action: CorporateAction::Split {
                new_units: d(new_units),
                old_units: d(old_units),
            },
        },
        cash,
    )
}

fn grant(id: &str, day: u32, quantity: &str, cost: Option<Money>) -> LedgerRecord {
    record(
        id,
        time(day, 10),
        EntryKind::Grant {
            instrument: stock("GALP.PT"),
            quantity: d(quantity),
            cost,
        },
        None,
    )
}

#[test]
fn the_lcid_reverse_split_realizes_the_fraction_against_the_newest_lot() {
    let buy_old = trade(
        "buy-40",
        1,
        Side::Buy,
        "40",
        Some(money("10", usd())),
        Some(money("-400", usd())),
    );
    let buy_new = trade(
        "buy-4",
        2,
        Side::Buy,
        "4",
        Some(money("10", usd())),
        Some(money("-40", usd())),
    );
    let reverse_split = split("split", 3, "1", "10", Some(money("1.23", usd())));
    let sell = trade(
        "sell-4",
        4,
        Side::Sell,
        "4",
        Some(money("20", usd())),
        Some(money("80", usd())),
    );

    let projection = project(
        &account(),
        &[buy_old, buy_new, reverse_split, sell],
        &[],
        bounds(),
    );

    assert!(projection.positions.is_empty());
    assert!(projection.cfd_positions.is_empty());
    assert_eq!(projection.realized.len(), 2);

    let fraction = &projection.realized[0];
    assert_eq!(fraction.record_id, "split");
    assert_eq!(fraction.quantity, d("0.4"));
    assert_eq!(fraction.proceeds, Some(money("1.23", usd())));
    assert_eq!(fraction.cost, Some(money("40", usd())));
    assert_eq!(fraction.result, Some(money("-38.77", usd())));

    let sale = &projection.realized[1];
    assert_eq!(sale.record_id, "sell-4");
    assert_eq!(sale.quantity, d("4"));
    assert_eq!(sale.proceeds, Some(money("80", usd())));
    assert_eq!(sale.cost, Some(money("400", usd())));
    assert_eq!(sale.result, Some(money("-320", usd())));

    assert_eq!(projection.cash, vec![money("-358.77", usd())]);
    assert!(projection.warnings.is_empty(), "{:?}", projection.warnings);
}

#[test]
fn a_forward_split_scales_quantities_and_keeps_the_total_cost() {
    let buy = trade(
        "buy",
        1,
        Side::Buy,
        "10",
        Some(money("30", usd())),
        Some(money("-300", usd())),
    );
    let forward_split = split("split", 2, "3", "1", None);

    let projection = project(&account(), &[buy, forward_split], &[], bounds());

    assert_eq!(projection.positions.len(), 1);
    let position = &projection.positions[0];
    assert_eq!(position.quantity, d("30"));
    assert_eq!(position.cost_basis, Some(money("300", usd())));
    assert_eq!(position.lots.len(), 1);
    assert_eq!(position.lots[0].quantity, d("30"));
    assert_eq!(position.lots[0].unit_cost, Some(money("10", usd())));
    assert!(projection.realized.is_empty());
}

#[test]
fn a_split_with_no_holding_warns_and_changes_nothing() {
    let orphan_split = split("split", 1, "2", "1", Some(money("5", usd())));

    let projection = project(&account(), &[orphan_split], &[], bounds());

    assert!(projection.positions.is_empty());
    assert!(projection.realized.is_empty());
    assert_eq!(
        projection
            .warnings
            .iter()
            .filter(|warning| warning.contains("LCID.US") && warning.contains("no open lots"))
            .count(),
        1,
        "warnings: {:?}",
        projection.warnings
    );
}

#[test]
fn a_split_after_the_as_of_bound_is_not_applied() {
    let buy = trade(
        "buy",
        1,
        Side::Buy,
        "40",
        Some(money("10", usd())),
        Some(money("-400", usd())),
    );
    let reverse_split = split("split", 10, "1", "10", Some(money("1.23", usd())));
    let as_of = AsOf {
        occurred_by: time(7, 0),
        known_by: time(31, 23),
    };

    let projection = project(&account(), &[buy, reverse_split], &[], as_of);

    assert_eq!(projection.records_used, 1);
    assert_eq!(projection.positions[0].quantity, d("40"));
    assert!(projection.realized.is_empty());
    assert_eq!(projection.cash, vec![money("-400", usd())]);
}

#[test]
fn a_zero_cost_grant_realizes_the_proceeds_and_warns() {
    let deposit = record(
        "deposit",
        time(1, 9),
        EntryKind::Deposit,
        Some(money("100", usd())),
    );
    let free_shares = grant("grant", 2, "10", None);
    let sale = record(
        "sell",
        time(3, 10),
        EntryKind::Trade {
            instrument: stock("GALP.PT"),
            side: Side::Sell,
            quantity: Some(d("4")),
            price: Some(money("12.5", usd())),
            position_ref: None,
        },
        Some(money("50", usd())),
    );

    let projection = project(&account(), &[deposit, free_shares, sale], &[], bounds());

    assert_eq!(projection.positions.len(), 1);
    let position = &projection.positions[0];
    assert_eq!(position.quantity, d("6"));
    assert_eq!(position.cost_basis, Some(money("0", usd())));
    assert_eq!(position.lots[0].origin, LotOrigin::Granted);
    assert_eq!(position.lots[0].unit_cost, Some(money("0", usd())));

    assert_eq!(projection.realized.len(), 1);
    assert_eq!(projection.realized[0].quantity, d("4"));
    assert_eq!(projection.realized[0].cost, Some(money("0", usd())));
    assert_eq!(projection.realized[0].proceeds, Some(money("50", usd())));
    assert_eq!(projection.realized[0].result, Some(money("50", usd())));

    assert!(
        projection
            .warnings
            .iter()
            .any(|warning| warning.contains("includes 6 units received free (zero cost basis)")),
        "warnings: {:?}",
        projection.warnings
    );
}

#[test]
fn a_grant_with_a_stated_cost_uses_it_as_the_basis() {
    let paid_grant = grant("grant", 1, "5", Some(money("50", usd())));

    let projection = project(&account(), &[paid_grant], &[], bounds());

    let position = &projection.positions[0];
    assert_eq!(position.quantity, d("5"));
    assert_eq!(position.cost_basis, Some(money("50", usd())));
    assert_eq!(position.lots[0].origin, LotOrigin::Granted);
    assert_eq!(position.lots[0].unit_cost, Some(money("10", usd())));
    assert!(projection.warnings.is_empty(), "{:?}", projection.warnings);
}

#[test]
fn a_grant_without_cash_has_no_cost_currency() {
    let free_shares = grant("grant", 1, "5", None);

    let projection = project(&account(), &[free_shares], &[], bounds());

    let position = &projection.positions[0];
    assert_eq!(position.quantity, d("5"));
    assert_eq!(position.cost_basis, None);
    assert_eq!(position.lots[0].unit_cost, None);
    assert!(
        projection
            .warnings
            .iter()
            .any(|warning| warning.contains("received free")),
        "warnings: {:?}",
        projection.warnings
    );
}

#[test]
fn split_day_derivative_and_fee_rows_do_not_change_owned_quantities() {
    let buy = trade(
        "buy",
        1,
        Side::Buy,
        "10",
        Some(money("10", usd())),
        Some(money("-100", usd())),
    );
    let derivative = record(
        "derivative",
        time(5, 9),
        EntryKind::DerivativeResult {
            instrument: stock("LCID.US"),
            position_ref: None,
        },
        Some(money("7", usd())),
    );
    let fee = record(
        "fee",
        time(5, 9),
        EntryKind::Fee {
            kind: FeeKind::Commission,
            instrument: Some(stock("LCID.US")),
        },
        Some(money("-1", usd())),
    );
    let forward_split = split("split", 5, "2", "1", None);

    let projection = project(
        &account(),
        &[buy, derivative, fee, forward_split],
        &[],
        bounds(),
    );

    assert_eq!(projection.positions.len(), 1);
    assert_eq!(projection.positions[0].quantity, d("20"));
    assert_eq!(
        projection.positions[0].cost_basis,
        Some(money("100", usd()))
    );
    assert!(projection.realized.is_empty());
    assert_eq!(projection.cash, vec![money("-94", usd())]);
    let labels: Vec<&str> = projection
        .flows
        .iter()
        .map(|flow| flow.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["derivative_result", "fee:commission", "trade:buy"]
    );
}
