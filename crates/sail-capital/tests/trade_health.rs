//! Round 19: data health, the split gate on stored prices, and membership snapshots.

use chrono::{Days, Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use sail_capital::contracts::{Currency, InstrumentKind};
use sail_capital::market::{Adjustment, PriceBar, PriceSeries};
use sail_capital::service::CapitalService;
use sail_capital::store::CapitalStore;
use sail_capital::trade::AssetClass;
use sail_capital::universe::ResearchMember;

fn price(value: f64) -> Decimal {
    Decimal::new((value * 100.0).round() as i64, 2)
}

fn service() -> (tempfile::TempDir, CapitalService) {
    let dir = tempfile::tempdir().unwrap();
    let service = CapitalService::open(dir.path()).unwrap();
    let cash = include_bytes!("fixtures/xtb/english_cash.csv");
    service
        .import_xtb(None, "cash.csv", cash, None, None, Utc::now())
        .unwrap();
    (dir, service)
}

fn series(closes: &[f64], end: NaiveDate) -> PriceSeries {
    let start = end - Days::new(closes.len() as u64 - 1);
    PriceSeries {
        source_id: "test".into(),
        symbol: "AAPL.US".into(),
        source_symbol: "AAPL".into(),
        currency: Some(Currency::parse("USD").unwrap()),
        adjustment: Adjustment::Unknown,
        bars: closes
            .iter()
            .enumerate()
            .map(|(index, close)| PriceBar {
                session: start + Days::new(index as u64),
                open: Some(price(*close)),
                high: Some(price(close * 1.01)),
                low: Some(price(close * 0.99)),
                close: price(*close),
                volume: None,
            })
            .collect(),
        payload_sha256: "split-test".into(),
    }
}

#[test]
fn health_flags_a_likely_split_and_sorts_it_first() {
    let (dir, service) = service();
    let today = Utc::now().date_naive();
    let mut closes = vec![400.0; 319];
    closes.push(101.0);
    let series = series(&closes, today);
    CapitalStore::open(dir.path())
        .unwrap()
        .record_prices("watch:AAPL.US", &series, Utc::now())
        .unwrap();
    let account = service.accounts().unwrap()[0].account.clone();
    let health = service.trade_health(&account).unwrap();

    let aapl = health
        .instruments
        .iter()
        .find(|instrument| instrument.symbol == "AAPL.US")
        .expect("AAPL.US is on the default watchlist");
    assert!(aapl.excluded);
    assert!(aapl.enough_bars);
    assert_eq!(aapl.bars, 320);
    assert_eq!(aapl.last_session, Some(today));
    assert_eq!(aapl.stale_weekdays, 0);
    assert!(!aapl.fetch_failed);
    assert_eq!(aapl.gaps.len(), 1);
    assert_eq!(aapl.gaps[0].likely_split, Some((4, 1)));
    assert_eq!(aapl.earnings_known, Some(false));
    assert_eq!(health.instruments[0].symbol, "AAPL.US");
    assert!(
        health
            .notes
            .iter()
            .any(|note| note.contains("takes an instrument out"))
    );
}

#[test]
fn health_marks_an_instrument_without_prices_as_short() {
    let (_dir, service) = service();
    let account = service.accounts().unwrap()[0].account.clone();
    let health = service.trade_health(&account).unwrap();
    let eurusd = health
        .instruments
        .iter()
        .find(|instrument| instrument.symbol == "EURUSD")
        .expect("a default watchlist row");
    assert!(!eurusd.fetch_failed, "fetching is off, so no fetch failed");
    assert!(!eurusd.enough_bars);
    assert_eq!(eurusd.bars, 0);
    assert_eq!(eurusd.earnings_known, None);
}

#[test]
fn membership_snapshots_are_append_only_and_the_latest_wins() {
    let dir = tempfile::tempdir().unwrap();
    let store = CapitalStore::open(dir.path()).unwrap();
    let now = Utc::now();
    let first = ResearchMember {
        yahoo: "AAPL".into(),
        name: "Apple".into(),
        class: AssetClass::Stock,
        membership: vec![(
            NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
            Some(NaiveDate::from_ymd_opt(2010, 1, 1).unwrap()),
        )],
    };
    store
        .record_research_members("sp500", &[first], now)
        .unwrap();
    let later = now + Duration::days(1);
    let second = ResearchMember {
        yahoo: "MSFT".into(),
        name: "Microsoft".into(),
        class: AssetClass::Stock,
        membership: vec![(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(), None)],
    };
    store
        .record_research_members("sp500", std::slice::from_ref(&second), later)
        .unwrap();
    let (at, members) = store.research_members("sp500").unwrap().unwrap();
    assert_eq!(at, later);
    assert_eq!(members, vec![second]);
    assert_eq!(store.research_members("stoxx50").unwrap(), None);
}

#[test]
fn a_held_position_gets_a_health_row_even_without_a_watchlist_entry() {
    let (_dir, service) = service();
    let account = service.accounts().unwrap()[0].account.clone();
    let portfolio = service
        .portfolio(&account, sail_capital::AsOf::now())
        .unwrap();
    let held = portfolio
        .positions
        .iter()
        .find(|position| {
            matches!(
                position.instrument.kind,
                InstrumentKind::Stock | InstrumentKind::Etf
            )
        })
        .map(|position| position.instrument.symbol.clone());
    let Some(symbol) = held else {
        return;
    };
    let health = service.trade_health(&account).unwrap();
    assert!(
        health
            .instruments
            .iter()
            .any(|instrument| instrument.symbol.eq_ignore_ascii_case(&symbol))
    );
}
