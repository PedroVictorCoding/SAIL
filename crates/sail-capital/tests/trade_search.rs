//! Asset search routing: Yahoo quote types become the classes the setups use, and a
//! source that cannot search is refused with a clear message. Synthetic, no network.

use async_trait::async_trait;
use sail_capital::market::{PriceSeries, PriceSource, PriceSourceError, SymbolMatch};
use sail_capital::service::{CapitalService, ServiceError};
use sail_capital::trade::AssetClass;
use std::sync::Arc;

struct FakeSearch;

#[async_trait]
impl PriceSource for FakeSearch {
    fn source_id(&self) -> &str {
        "yfinance"
    }

    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError> {
        Err(PriceSourceError::UnknownSymbol(symbol.to_string()))
    }

    async fn search(&self, query: &str) -> Result<Vec<SymbolMatch>, PriceSourceError> {
        assert_eq!(query, "apple");
        Ok(vec![
            SymbolMatch {
                symbol: "AAPL".into(),
                name: "Apple Inc.".into(),
                exchange: "NASDAQ".into(),
                quote_type: "EQUITY".into(),
            },
            SymbolMatch {
                symbol: "APLE".into(),
                name: "Apple Hospitality".into(),
                exchange: "NYSE".into(),
                quote_type: "EQUITY".into(),
            },
            SymbolMatch {
                symbol: "BONDX".into(),
                name: "A bond fund".into(),
                exchange: "X".into(),
                quote_type: "BOND".into(),
            },
        ])
    }
}

#[tokio::test]
async fn search_maps_yahoo_types_and_drops_unsupported_ones() {
    let dir = tempfile::tempdir().unwrap();
    let service = CapitalService::open(dir.path())
        .unwrap()
        .with_prices(Arc::new(FakeSearch), true);
    let results = service.search_trade_symbols(" apple ").await.unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].symbol, "AAPL");
    assert_eq!(results[0].name, "Apple Inc.");
    assert_eq!(results[0].class, AssetClass::Stock);
    assert_eq!(results[1].symbol, "APLE");
}

#[tokio::test]
async fn a_source_without_search_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let service = CapitalService::open(dir.path())
        .unwrap()
        .with_prices(Arc::new(sail_capital::market::DisabledSource), false);
    let error = service.search_trade_symbols("apple").await.unwrap_err();
    match error {
        ServiceError::BadRequest(message) => {
            assert!(message.contains("Price fetching is off"), "{message}");
        }
        other => panic!("unexpected error: {other}"),
    }
}
