//! Round 13: watchlist, empty signal states, and open/high/low backfill.
use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use sail_capital::market::*;
use sail_capital::service::{CapitalService, ServiceError};
use sail_capital::trade::AssetClass;
use sail_capital::universe::{WatchChange, WatchItem, default_watchlist};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const FILE: &[u8] = include_bytes!("fixtures/xtb/xtb_2026_layout.xlsx");

fn setup() -> (tempfile::TempDir, CapitalService, sail_capital::AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();
    let account = svc
        .import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap()
        .account;
    (dir, svc, account)
}

#[test]
fn the_watchlist_starts_from_defaults_and_keeps_changes() {
    let (_dir, svc, _account) = setup();
    assert_eq!(svc.watchlist().unwrap(), default_watchlist());
    let item = WatchItem {
        symbol: " pltr.us ".into(),
        yahoo: "PLTR".into(),
        class: AssetClass::Stock,
        currency: "usd".into(),
        name: "Palantir".into(),
    };
    let list = svc
        .change_watchlist(WatchChange::Add { item }, Utc::now())
        .unwrap();
    let added = list.iter().find(|w| w.symbol == "PLTR.US").unwrap();
    assert_eq!(added.currency, "USD");
    let list = svc
        .change_watchlist(
            WatchChange::Remove {
                symbol: "eurusd".into(),
            },
            Utc::now(),
        )
        .unwrap();
    assert!(list.iter().all(|w| w.symbol != "EURUSD"));
    assert!(matches!(
        svc.change_watchlist(
            WatchChange::Remove {
                symbol: "NOPE".into()
            },
            Utc::now()
        ),
        Err(ServiceError::BadRequest(_))
    ));
    let bad = WatchItem {
        symbol: "A B".into(),
        yahoo: "X".into(),
        class: AssetClass::Stock,
        currency: "USD".into(),
        name: String::new(),
    };
    assert!(matches!(
        svc.change_watchlist(WatchChange::Add { item: bad }, Utc::now()),
        Err(ServiceError::BadRequest(_))
    ));
}

#[test]
fn before_any_run_the_views_are_empty_and_say_why() {
    let (_dir, svc, account) = setup();
    let view = svc.trade_view(&account).unwrap();
    assert!(view.day.is_none());
    assert!(view.next_run_at > Utc::now());
    assert!(view.notes[0].contains("Price fetching is off"));
    let record = svc.trade_record(&account).unwrap();
    assert!(record.cards.is_empty());
    assert_eq!(record.totals.closed, 0);
    assert!(svc.trade_guards(&account).unwrap().is_empty());
}

/// Gives bars without open/high/low until `ohl` is set, as the source did before
/// round 12 stored them.
struct Bars {
    ohl: AtomicBool,
}

#[async_trait]
impl PriceSource for Bars {
    fn source_id(&self) -> &str {
        "fake"
    }

    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError> {
        let start = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let with = self.ohl.load(Ordering::SeqCst);
        let bars = (0..5)
            .map(|i| {
                let close = Decimal::from(100 + i);
                PriceBar {
                    session: start + Duration::days(i),
                    open: with.then_some(close),
                    high: with.then_some(close + Decimal::ONE),
                    low: with.then_some(close - Decimal::ONE),
                    close,
                    volume: None,
                }
            })
            .collect();
        Ok(PriceSeries {
            source_id: "fake".into(),
            symbol: symbol.into(),
            source_symbol: symbol.into(),
            currency: None,
            adjustment: Adjustment::SplitAdjusted,
            bars,
            payload_sha256: "x".into(),
        })
    }
}

#[tokio::test]
async fn stored_bars_gain_open_high_low_once_and_keep_their_first_time() {
    let dir = tempfile::tempdir().unwrap();
    let source = Arc::new(Bars {
        ohl: AtomicBool::new(false),
    });
    let svc = CapitalService::open(dir.path())
        .unwrap()
        .with_prices(source.clone(), true);
    let account = svc
        .import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap()
        .account;
    svc.refresh_prices(Some(&account)).await.unwrap();
    let store = sail_capital::store::CapitalStore::open(dir.path()).unwrap();
    let key = svc.price_status().unwrap().per_instrument[0]
        .instrument_key
        .clone();
    let before = store.prices(&key, Utc::now()).unwrap();
    assert!(before.iter().all(|o| o.ohl.is_none()));

    source.ohl.store(true, Ordering::SeqCst);
    svc.refresh_prices(Some(&account)).await.unwrap();
    let after = store.prices(&key, Utc::now()).unwrap();
    assert_eq!(after.len(), before.len());
    assert!(after.iter().all(|o| o.ohl.is_some()));
    for (b, a) in before.iter().zip(&after) {
        assert_eq!(b.first_available_at, a.first_available_at);
    }
    // A third refresh writes nothing new.
    let version = store.price_version().unwrap();
    svc.refresh_prices(Some(&account)).await.unwrap();
    assert_eq!(store.price_version().unwrap(), version);
}
