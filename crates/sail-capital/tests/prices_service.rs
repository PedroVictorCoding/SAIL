//! Price store and refresh through the service, with a fake source (no network).
use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use sail_capital::market::*;
use sail_capital::service::CapitalService;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

const FILE: &[u8] = include_bytes!("fixtures/xtb/xtb_2026_layout.xlsx");

struct FakePrices {
    calls: AtomicU32,
    last_close: Mutex<Decimal>,
    refuse: bool,
}

#[async_trait]
impl PriceSource for FakePrices {
    fn source_id(&self) -> &str {
        "fake"
    }

    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.refuse {
            return Err(PriceSourceError::Refused(429));
        }
        let start = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let mut bars: Vec<PriceBar> = (0..10)
            .map(|i| PriceBar {
                session: start + Duration::days(i),
                open: None,
                high: None,
                low: None,
                close: Decimal::new(100 + i, 0),
                volume: None,
            })
            .collect();
        bars.last_mut().unwrap().close = *self.last_close.lock().unwrap();
        Ok(PriceSeries {
            source_id: "fake".into(),
            symbol: symbol.into(),
            source_symbol: symbol.to_lowercase(),
            currency: None,
            adjustment: Adjustment::Unknown,
            bars,
            payload_sha256: "x".into(),
        })
    }
}

fn fake(refuse: bool) -> Arc<FakePrices> {
    Arc::new(FakePrices {
        calls: AtomicU32::new(0),
        last_close: Mutex::new(Decimal::new(109, 0)),
        refuse,
    })
}

fn service(dir: &std::path::Path, prices: Arc<FakePrices>, configured: bool) -> CapitalService {
    let svc = CapitalService::open(dir)
        .unwrap()
        .with_prices(prices, configured);
    svc.import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap();
    svc
}

#[tokio::test]
async fn prices_are_never_fetched_without_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let source = fake(false);
    let svc = service(dir.path(), source.clone(), false);
    let status = svc.refresh_prices(None).await.unwrap();
    assert!(!status.configured);
    assert!(status.last_error.unwrap().contains("SAIL_PRICES_SOURCE"));
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert_eq!(status.instruments, 0);
}

#[tokio::test]
async fn refresh_is_idempotent_and_revisions_keep_first_availability() {
    let dir = tempfile::tempdir().unwrap();
    let source = fake(false);
    let svc = service(dir.path(), source.clone(), true);
    let before = Utc::now() - Duration::seconds(1);

    let status = svc.refresh_prices(None).await.unwrap();
    assert_eq!(status.last_error, None);
    // Two held owned instruments (AAPL.US, TTE.FR) × 10 sessions.
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    assert_eq!((status.instruments, status.observations), (2, 20));

    let first = svc.prices("AAPL.US", None, None).unwrap();
    assert_eq!(first.len(), 10);
    let first_seen = first.last().unwrap().first_available_at;
    assert!(first_seen >= before);

    // Same data again: nothing new.
    let again = svc.refresh_prices(None).await.unwrap();
    assert_eq!(again.observations, 20);

    // The source revises the last close: the new value wins, first availability is kept.
    *source.last_close.lock().unwrap() = Decimal::new(1095, 1);
    svc.refresh_prices(None).await.unwrap();
    let revised = svc.prices("AAPL.US", None, None).unwrap();
    assert_eq!(revised.len(), 10);
    assert_eq!(revised.last().unwrap().close, Decimal::new(1095, 1));
    assert_eq!(revised.last().unwrap().first_available_at, first_seen);

    // Range filter.
    let some = svc
        .prices("AAPL.US", NaiveDate::from_ymd_opt(2025, 1, 5), None)
        .unwrap();
    assert_eq!(some.len(), 7);
}

#[tokio::test]
async fn prices_known_later_are_invisible_earlier() {
    let dir = tempfile::tempdir().unwrap();
    let store = sail_capital::store::CapitalStore::open(dir.path()).unwrap();
    let series = fake(false).daily("AAPL.US").await.unwrap();
    let stored_at = Utc::now();
    store
        .record_prices("sym:AAPL.US", &series, stored_at)
        .unwrap();
    assert!(
        store
            .prices("sym:AAPL.US", stored_at - Duration::seconds(1))
            .unwrap()
            .is_empty(),
        "asked as of before SAIL had them, there are no prices"
    );
    assert_eq!(store.prices("sym:AAPL.US", stored_at).unwrap().len(), 10);
}

#[tokio::test]
async fn forecasts_cover_every_held_instrument_with_prices() {
    let dir = tempfile::tempdir().unwrap();
    let svc = service(dir.path(), fake(false), true);
    svc.refresh_prices(None).await.unwrap();
    let account = svc.accounts().unwrap()[0].account.clone();
    let view = svc
        .forecasts(&account, 20, sail_capital::forecast::ModelId::NoChange)
        .unwrap();
    assert_eq!(view.forecasts.len(), 2);
    assert!(view.without_prices.is_empty());
    // Ten sessions is far from a year of data: no numbers, only the reason.
    for f in &view.forecasts {
        assert!(matches!(
            f.status,
            sail_capital::forecast::ForecastStatus::InsufficientEvidence { .. }
        ));
    }
}

#[tokio::test]
async fn a_refusal_stops_the_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let source = fake(true);
    let svc = service(dir.path(), source.clone(), true);
    let status = svc.refresh_prices(None).await.unwrap();
    assert!(status.last_error.unwrap().contains("429"));
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn bad_horizons_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let svc = service(dir.path(), fake(false), true);
    let account = svc.accounts().unwrap()[0].account.clone();
    assert!(
        svc.forecasts(&account, 7, sail_capital::forecast::ModelId::NoChange)
            .is_err()
    );
}
