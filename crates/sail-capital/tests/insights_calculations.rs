//! Tests for portfolio insights (Antigravity's slice, round 9).

use std::str::FromStr;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use sail_capital::AsOf;
use sail_capital::contracts::{
    AccountId, Currency, EntryKind, EventTime, Instrument, InstrumentKind, LedgerRecord, Money,
    ParsedEntry, RowRef, SheetKind, TaxKind, TimePrecision,
};
use sail_capital::insights::{Concentration, insights};
use sail_capital::ledger::{
    DividendView, FlowTotal, Position, Projection, Reconciliation, ReconciliationStatus,
};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).expect("valid decimal")
}

fn usd() -> Currency {
    Currency::parse("USD").expect("valid USD")
}

fn eur() -> Currency {
    Currency::eur()
}

fn make_projection(
    positions: Vec<Position>,
    cash: Vec<Money>,
    dividends: Vec<DividendView>,
    flows: Vec<FlowTotal>,
    snapshot_as_of: Option<chrono::DateTime<Utc>>,
) -> Projection {
    Projection {
        account: AccountId::from_stored("xtb:test"),
        as_of: AsOf::now(),
        positions,
        cfd_positions: Vec::new(),
        cash,
        dividends,
        realized: Vec::new(),
        flows,
        snapshot_as_of,
        reconciliation: Reconciliation {
            status: ReconciliationStatus::Unverified,
            variances: Vec::new(),
            compared: 0,
        },
        warnings: Vec::new(),
        records_used: 0,
    }
}

fn make_position(symbol: &str, kind: InstrumentKind, market_value: Option<Money>) -> Position {
    Position {
        instrument: Instrument {
            symbol: symbol.into(),
            isin: None,
            name: Some(symbol.into()),
            currency: market_value.as_ref().map(|m| m.currency.clone()),
            kind,
        },
        quantity: d("10"),
        lots: Vec::new(),
        cost_basis: market_value.clone(),
        broker_market_value: market_value,
    }
}

fn event_time(dt: chrono::DateTime<Utc>) -> EventTime {
    EventTime {
        at: dt,
        precision: TimePrecision::Exact,
    }
}

#[test]
fn mixed_currencies_leaves_allocation_and_concentration_none() {
    let now = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
    let pos_usd = make_position(
        "AAPL.US",
        InstrumentKind::Stock,
        Some(Money::new(d("1000"), usd())),
    );
    let pos_eur = make_position(
        "SAP.DE",
        InstrumentKind::Stock,
        Some(Money::new(d("800"), eur())),
    );

    let proj = make_projection(vec![pos_usd, pos_eur], vec![], vec![], vec![], None);
    let ins = insights(&proj, &[], now);

    assert_eq!(ins.currency, None);
    assert_eq!(ins.owned_value, None);
    assert!(ins.allocation.is_empty());
    assert!(ins.by_kind.is_empty());
    assert_eq!(ins.concentration, None);
    assert!(
        ins.warnings
            .iter()
            .any(|w| w.contains("multiple currencies")),
        "should warn about multiple currencies: {:?}",
        ins.warnings
    );
}

#[test]
fn unvalued_position_is_listed_and_omitted_from_allocation() {
    let now = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
    let pos_valued = make_position(
        "AAPL.US",
        InstrumentKind::Stock,
        Some(Money::new(d("1000"), usd())),
    );
    let pos_unvalued = make_position("GOOG.US", InstrumentKind::Stock, None);

    let proj = make_projection(vec![pos_valued, pos_unvalued], vec![], vec![], vec![], None);
    let ins = insights(&proj, &[], now);

    assert_eq!(ins.unvalued, vec!["GOOG.US"]);
    assert_eq!(ins.currency, Some(usd()));
    assert_eq!(ins.owned_value, Some(Money::new(d("1000"), usd())));
    assert_eq!(ins.allocation.len(), 1);
    assert_eq!(ins.allocation[0].symbol, "AAPL.US");
    assert_eq!(ins.allocation[0].share, d("1.0000"));

    let conc = ins.concentration.expect("concentration computed");
    assert_eq!(conc.positions, 1);
    assert_eq!(conc.largest_symbol, "AAPL.US");
    assert_eq!(conc.largest_share, d("1.0000"));
}

#[test]
fn cfd_dividends_are_excluded_from_income() {
    let now = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
    let dt = Utc.with_ymd_and_hms(2025, 3, 15, 10, 0, 0).unwrap();

    let stock_inst = Instrument {
        symbol: "AAPL.US".into(),
        isin: None,
        name: Some("Apple".into()),
        currency: Some(usd()),
        kind: InstrumentKind::Stock,
    };
    let cfd_inst = Instrument {
        symbol: "OIL".into(),
        isin: None,
        name: Some("Oil CFD".into()),
        currency: Some(usd()),
        kind: InstrumentKind::Cfd,
    };

    let div_stock = DividendView {
        instrument: Some(stock_inst),
        paid: event_time(dt),
        gross: Money::new(d("10.00"), usd()),
        withheld: Some(Money::new(d("1.50"), usd())),
        net: Some(Money::new(d("8.50"), usd())),
        record_ids: vec!["rec_div1".into()],
    };
    let div_cfd = DividendView {
        instrument: Some(cfd_inst),
        paid: event_time(dt),
        gross: Money::new(d("5.00"), usd()),
        withheld: None,
        net: Some(Money::new(d("5.00"), usd())),
        record_ids: vec!["rec_div2".into()],
    };

    let proj = make_projection(vec![], vec![], vec![div_stock, div_cfd], vec![], None);
    let ins = insights(&proj, &[], now);

    assert_eq!(ins.income.by_month.len(), 1);
    let m = &ins.income.by_month[0];
    assert_eq!(m.month, "2025-03");
    assert_eq!(m.dividends_gross.amount, d("10.00"));
    assert_eq!(m.withheld.amount, d("-1.50"));
    assert_eq!(m.net.amount, d("8.50"));
}

#[test]
fn trailing_12m_boundary_excludes_older_months() {
    let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
    // 12 months before now (2025-06-15) is 2024-06-15.
    // Month 2024-05: start 2024-05-01 is < 2024-06-15 -> EXCLUDED
    // Month 2024-06: start 2024-06-01 is < 2024-06-15 -> EXCLUDED
    // Month 2024-07: start 2024-07-01 is >= 2024-06-15 -> INCLUDED
    // Month 2025-06: start 2025-06-01 is >= 2024-06-15 -> INCLUDED

    let mk_div = |year, month, day, amount| {
        let dt = Utc.with_ymd_and_hms(year, month, day, 10, 0, 0).unwrap();
        DividendView {
            instrument: None,
            paid: event_time(dt),
            gross: Money::new(d(amount), usd()),
            withheld: None,
            net: Some(Money::new(d(amount), usd())),
            record_ids: vec!["rec".into()],
        }
    };

    let divs = vec![
        mk_div(2024, 5, 20, "100.00"),
        mk_div(2024, 6, 20, "100.00"),
        mk_div(2024, 7, 20, "100.00"),
        mk_div(2025, 6, 5, "100.00"),
    ];

    let proj = make_projection(vec![], vec![], divs, vec![], None);
    let ins = insights(&proj, &[], now);

    assert_eq!(ins.income.by_month.len(), 4);
    assert_eq!(ins.income.trailing_12m_net.len(), 1);
    assert_eq!(
        ins.income.trailing_12m_net[0].amount,
        d("200.00"),
        "only 2024-07 and 2025-06 should be included in trailing 12m"
    );
}

#[test]
fn hhi_for_1_2_and_4_equal_positions() {
    let now = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();

    // 1 position
    let p1 = make_position(
        "A",
        InstrumentKind::Stock,
        Some(Money::new(d("1000"), usd())),
    );
    let proj1 = make_projection(vec![p1], vec![], vec![], vec![], None);
    let ins1 = insights(&proj1, &[], now);
    let c1: Concentration = ins1.concentration.unwrap();
    assert_eq!(c1.hhi, d("1.0000"));
    assert_eq!(c1.effective_positions, d("1.0"));
    assert_eq!(c1.largest_share, d("1.0000"));
    assert_eq!(c1.top_n_share, d("1.0000"));

    // 2 equal positions
    let p2a = make_position(
        "A",
        InstrumentKind::Stock,
        Some(Money::new(d("500"), usd())),
    );
    let p2b = make_position(
        "B",
        InstrumentKind::Stock,
        Some(Money::new(d("500"), usd())),
    );
    let proj2 = make_projection(vec![p2a, p2b], vec![], vec![], vec![], None);
    let ins2 = insights(&proj2, &[], now);
    let c2 = ins2.concentration.unwrap();
    assert_eq!(c2.hhi, d("0.5000"));
    assert_eq!(c2.effective_positions, d("2.0"));
    assert_eq!(c2.largest_share, d("0.5000"));
    assert_eq!(c2.top_n_share, d("1.0000"));

    // 4 equal positions
    let p4a = make_position(
        "A",
        InstrumentKind::Stock,
        Some(Money::new(d("250"), usd())),
    );
    let p4b = make_position(
        "B",
        InstrumentKind::Stock,
        Some(Money::new(d("250"), usd())),
    );
    let p4c = make_position(
        "C",
        InstrumentKind::Stock,
        Some(Money::new(d("250"), usd())),
    );
    let p4d = make_position(
        "D",
        InstrumentKind::Stock,
        Some(Money::new(d("250"), usd())),
    );
    let proj4 = make_projection(vec![p4a, p4b, p4c, p4d], vec![], vec![], vec![], None);
    let ins4 = insights(&proj4, &[], now);
    let c4 = ins4.concentration.unwrap();
    assert_eq!(c4.hhi, d("0.2500"));
    assert_eq!(c4.effective_positions, d("4.0"));
    assert_eq!(c4.largest_share, d("0.2500"));
    assert_eq!(c4.top_n_share, d("1.0000"));
}

#[test]
fn zero_net_deposits_omits_gain_ratio() {
    let now = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();

    let flows = vec![
        FlowTotal {
            label: "deposit".into(),
            total: Money::new(d("1000.00"), usd()),
            count: 1,
        },
        FlowTotal {
            label: "withdrawal".into(),
            total: Money::new(d("-1000.00"), usd()),
            count: 1,
        },
    ];

    let pos = make_position(
        "AAPL.US",
        InstrumentKind::Stock,
        Some(Money::new(d("800.00"), usd())),
    );
    let cash = vec![Money::new(d("200.00"), usd())];

    let proj = make_projection(vec![pos], cash, vec![], flows, None);
    let ins = insights(&proj, &[], now);

    let tr = ins.total_return.expect("total return computed");
    assert_eq!(tr.net_deposits.amount, d("0.00"));
    assert_eq!(tr.current_value.amount, d("1000.00"));
    assert_eq!(tr.gain.amount, d("1000.00"));
    assert_eq!(
        tr.gain_ratio, None,
        "gain_ratio is None when net_deposits <= 0"
    );
}

#[test]
fn interest_and_freshness_calculations() {
    let now = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
    let snap_time = now - chrono::Duration::hours(80);

    let rec_interest = LedgerRecord {
        id: "rec1".into(),
        account: AccountId::from_stored("xtb:test"),
        batch_id: "batch1".into(),
        ingested_at: now,
        entry: ParsedEntry {
            natural_key: "nat1".into(),
            source: RowRef {
                sheet: None,
                sheet_kind: SheetKind::CashOperations,
                row: 1,
            },
            broker_ref: Some("ref1".into()),
            occurred: event_time(now - chrono::Duration::days(10)),
            kind: EntryKind::Interest,
            cash_effect: Some(Money::new(d("10.00"), usd())),
            comment: None,
        },
    };

    let rec_tax = LedgerRecord {
        id: "rec2".into(),
        account: AccountId::from_stored("xtb:test"),
        batch_id: "batch1".into(),
        ingested_at: now,
        entry: ParsedEntry {
            natural_key: "nat2".into(),
            source: RowRef {
                sheet: None,
                sheet_kind: SheetKind::CashOperations,
                row: 2,
            },
            broker_ref: Some("ref2".into()),
            occurred: event_time(now - chrono::Duration::days(10)),
            kind: EntryKind::Tax {
                kind: TaxKind::OnInterest,
                instrument: None,
            },
            cash_effect: Some(Money::new(d("-1.50"), usd())),
            comment: None,
        },
    };

    let proj = make_projection(vec![], vec![], vec![], vec![], Some(snap_time));
    let ins = insights(&proj, &[rec_interest, rec_tax], now);

    assert_eq!(ins.income.by_month.len(), 1);
    assert_eq!(ins.income.by_month[0].interest_net.amount, d("8.50"));
    assert_eq!(ins.freshness.snapshot_age_hours, Some(80));
    assert!(ins.freshness.stale, "80h > 72h should be stale");
}
