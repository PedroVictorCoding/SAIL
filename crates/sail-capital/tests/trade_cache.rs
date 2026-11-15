//! Round 19 replay cache: exact prefixes, hashes, costs, storage, the service and timing.

mod trade_support;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sail_capital::contracts::{AccountId, Currency};
use sail_capital::market::{Adjustment, PriceBar, PriceSeries, PriceSource, PriceSourceError};
use sail_capital::service::CapitalService;
use sail_capital::store::{CapitalStore, ReplayCache};
use sail_capital::trade::cache::{costs_tag, cut_session, merge, prefix_hash};
use sail_capital::trade::{
    AssetClass, Bar, Costs, Exit, SetupId, Side, Trade, replay, replay_from,
};
use sail_capital::universe::{WatchChange, WatchItem};
use trade_support::{bars_with_prior_open, instrument, random_walk};

fn history(index: usize, length: usize) -> Vec<Bar> {
    let (closes, _) = random_walk(length, 0.0, 0.012, 2000 + index as u64);
    bars_with_prior_open(&closes, 0.5)
}

fn trade(session: NaiveDate) -> Trade {
    Trade {
        key: "test".into(),
        setup: SetupId::ThreeDayReversal,
        side: Side::Long,
        signal_session: session,
        entry_session: session,
        entry: 100.0,
        stop: 95.0,
        exit_session: session,
        exit: Exit::Target1,
        r_multiple: 0.0,
        dividend_r: 0.0,
        ex_date_gap: false,
        regime: None,
    }
}

fn day(index: u64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Days::new(index)
}

#[test]
fn merge_equals_a_full_replay_on_twenty_random_walks() {
    let costs = Costs::default_for(AssetClass::Stock);
    for index in 0..20 {
        let old_bars = history(index, 500);
        let old = instrument("test", "TST", AssetClass::Stock, false, old_bars.clone());
        let old_trades = replay(&old, costs);
        let cut = cut_session(&old_bars).expect("a cut");
        let cached: Vec<Trade> = old_trades
            .iter()
            .filter(|trade| trade.signal_session < cut)
            .cloned()
            .collect();
        for extra in [1usize, 5, 30] {
            let current_bars = history(index, 500 + extra);
            let current = instrument("test", "TST", AssetClass::Stock, false, current_bars);
            let merged = merge(cached.clone(), replay_from(&current, costs, cut), cut);
            let full = replay(&current, costs);
            assert_eq!(merged, full, "instrument {index} after {extra} bars");
        }
    }
}

#[test]
fn a_change_to_any_earlier_bar_changes_the_prefix_hash() {
    let mut bars = history(0, 300);
    let cut = cut_session(&bars).expect("a cut");
    let before = prefix_hash(&bars, cut);
    let last = bars.len() - 1;
    bars[last].close += 1.0;
    assert_eq!(prefix_hash(&bars, cut), before);
    bars[0].close += 1.0;
    assert_ne!(prefix_hash(&bars, cut), before);
}

#[test]
fn a_costs_change_changes_the_tag() {
    let costs = Costs::default_for(AssetClass::Stock);
    assert_eq!(costs_tag(costs), costs_tag(costs));
    let changed = Costs {
        round_trip: costs.round_trip + 0.0001,
        ..costs
    };
    assert_ne!(costs_tag(costs), costs_tag(changed));
}

#[test]
fn the_store_round_trips_and_replaces_a_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let store = CapitalStore::open(dir.path()).unwrap();
    let now: DateTime<Utc> = "2026-01-02T00:00:00Z".parse().unwrap();
    let first = ReplayCache {
        key: "test".into(),
        costs_tag: "tag".into(),
        cut: day(10),
        prefix_hash: "aaa".into(),
        trades: vec![Trade {
            r_multiple: 1.031_876_938_273_007_7,
            ..trade(day(1))
        }],
    };
    store
        .record_replay_caches(std::slice::from_ref(&first), now)
        .unwrap();
    assert_eq!(
        store.replay_cache("test", "tag").unwrap(),
        Some(first.clone())
    );
    assert_eq!(store.replay_cache("test", "other").unwrap(), None);
    assert_eq!(store.replay_cache("missing", "tag").unwrap(), None);

    let second = ReplayCache {
        prefix_hash: "bbb".into(),
        trades: vec![trade(day(2)), trade(day(3))],
        ..first
    };
    store
        .record_replay_caches(std::slice::from_ref(&second), now)
        .unwrap();
    assert_eq!(store.replay_cache("test", "tag").unwrap(), Some(second));
}

struct FakePrices {
    bars: Vec<PriceBar>,
}

#[async_trait]
impl PriceSource for FakePrices {
    fn source_id(&self) -> &str {
        "fake"
    }

    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError> {
        Ok(PriceSeries {
            source_id: "fake".into(),
            symbol: symbol.into(),
            source_symbol: symbol.to_ascii_lowercase(),
            currency: Some(Currency::parse("USD").unwrap()),
            adjustment: Adjustment::Unknown,
            bars: self.bars.clone(),
            payload_sha256: "synthetic".into(),
        })
    }
}

fn price_bars(count: usize) -> Vec<PriceBar> {
    let (closes, ranges) = random_walk(count, 0.0, 0.012, 77);
    closes
        .iter()
        .zip(ranges)
        .enumerate()
        .map(|(index, (close, range))| {
            let price = |value: f64| Decimal::new((value * 100.0).round() as i64, 2);
            PriceBar {
                session: day(index as u64),
                open: Some(price(*close)),
                high: Some(price(close + range)),
                low: Some(price(close - range)),
                close: price(*close),
                volume: None,
            }
        })
        .collect()
}

async fn synthetic_service() -> (tempfile::TempDir, CapitalService, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let service = CapitalService::open(dir.path()).unwrap().with_prices(
        Arc::new(FakePrices {
            bars: price_bars(320),
        }),
        true,
    );
    let cash = include_bytes!("fixtures/xtb/english_cash.csv");
    service
        .import_xtb(None, "cash.csv", cash, None, None, Utc::now())
        .unwrap();
    let now = Utc::now();
    for item in service.watchlist().unwrap() {
        service
            .change_watchlist(
                WatchChange::Remove {
                    symbol: item.symbol,
                },
                now,
            )
            .unwrap();
    }
    service
        .change_watchlist(
            WatchChange::Add {
                item: WatchItem {
                    symbol: "TESTFX".into(),
                    yahoo: "TESTFX=X".into(),
                    class: AssetClass::Fx,
                    currency: "USD".into(),
                    name: "Test pair".into(),
                },
            },
            now,
        )
        .unwrap();
    let account = service.accounts().unwrap()[0].account.clone();
    (dir, service, account)
}

#[tokio::test]
async fn the_service_misses_then_hits_and_keeps_the_same_cards() {
    let (_dir, service, account) = synthetic_service().await;
    let first = service.run_trade_signals(&account).await.unwrap();
    let first_day = first.day.expect("a first run");
    let first_stats = first_day.run_stats.expect("run stats");
    assert_eq!(first_stats.replay_cache_hits, 0);
    assert!(first_stats.replay_cache_misses > 0);

    let second = service.run_trade_signals(&account).await.unwrap();
    let second_day = second.day.expect("a second run");
    let second_stats = second_day.run_stats.expect("run stats");
    assert_eq!(
        second_stats.replay_cache_hits,
        first_stats.replay_cache_misses
    );
    assert_eq!(second_stats.replay_cache_misses, 0);
    assert_eq!(first_day.cards, second_day.cards);
    assert_eq!(first_day.candidates, second_day.candidates);
    assert_eq!(first_day.scoreboard, second_day.scoreboard);
    assert_eq!(first_day.session, second_day.session);
}

#[test]
#[ignore = "timing test; run with --release --ignored --nocapture"]
fn a_cached_rerun_is_at_least_twice_as_fast() {
    let count = 200;
    let length = 2100;
    let costs = Costs::default_for(AssetClass::Stock);
    let old: Vec<_> = (0..count)
        .map(|index| {
            instrument(
                "test",
                "TST",
                AssetClass::Stock,
                false,
                history(index, length),
            )
        })
        .collect();
    let current: Vec<_> = (0..count)
        .map(|index| {
            instrument(
                "test",
                "TST",
                AssetClass::Stock,
                false,
                history(index, length + 1),
            )
        })
        .collect();
    let cached: Vec<(NaiveDate, Vec<Trade>)> = old
        .iter()
        .map(|instrument| {
            let cut = cut_session(&instrument.bars).expect("a cut");
            let prefix = replay(instrument, costs)
                .into_iter()
                .filter(|trade| trade.signal_session < cut)
                .collect();
            (cut, prefix)
        })
        .collect();

    let start = Instant::now();
    let full: Vec<Vec<Trade>> = current
        .iter()
        .map(|instrument| replay(instrument, costs))
        .collect();
    let full_time = start.elapsed();

    let start = Instant::now();
    let merged: Vec<Vec<Trade>> = current
        .iter()
        .zip(&cached)
        .map(|(instrument, (cut, prefix))| {
            merge(prefix.clone(), replay_from(instrument, costs, *cut), *cut)
        })
        .collect();
    let cached_time = start.elapsed();

    println!(
        "full replay: {full_time:?} over {} trades; cached rerun: {cached_time:?}; speed-up {:.1}x",
        full.iter().map(Vec::len).sum::<usize>(),
        full_time.as_secs_f64() / cached_time.as_secs_f64()
    );
    assert_eq!(merged, full);
    // Indicators run over every bar so a cached rerun stays bit-exact (running sums
    // round differently from a shorter start); only the per-session conditions are
    // skipped, which bounds the gain at roughly 2-3x.
    assert!(
        cached_time.as_secs_f64() * 2.0 <= full_time.as_secs_f64(),
        "cached rerun was not 2x faster"
    );
}
