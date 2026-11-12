//! The signal universe (round 13): the owner's holdings plus a watchlist of liquid
//! instruments XTB offers. Fetching their prices sends only these symbols to Yahoo.
//!
//! The watchlist starts from [`DEFAULT_WATCHLIST`]. The owner can add or remove entries.
//! Each change is stored append-only, and the current list is the replay of those events.
use crate::trade::AssetClass;
use serde::{Deserialize, Serialize};

/// One watchlist entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchItem {
    /// The symbol as XTB shows it, e.g. `EURAUD`, `US500`, `NVDA.US`.
    pub symbol: String,
    /// Yahoo's symbol, e.g. `EURAUD=X`, `^GSPC`, `NVDA`.
    pub yahoo: String,
    /// Asset class.
    pub class: AssetClass,
    /// Trading currency (ISO code).
    pub currency: String,
    /// Display name.
    pub name: String,
}

const fn w(
    symbol: &'static str,
    yahoo: &'static str,
    class: AssetClass,
    currency: &'static str,
    name: &'static str,
) -> (
    &'static str,
    &'static str,
    AssetClass,
    &'static str,
    &'static str,
) {
    (symbol, yahoo, class, currency, name)
}

/// The starting watchlist: the most liquid instruments in each class XTB offers.
pub const DEFAULT_WATCHLIST: [(&str, &str, AssetClass, &str, &str); 44] = [
    // FX majors and liquid crosses (CFDs at XTB).
    w(
        "EURUSD",
        "EURUSD=X",
        AssetClass::Fx,
        "USD",
        "Euro / US dollar",
    ),
    w(
        "GBPUSD",
        "GBPUSD=X",
        AssetClass::Fx,
        "USD",
        "Pound / US dollar",
    ),
    w(
        "USDJPY",
        "USDJPY=X",
        AssetClass::Fx,
        "JPY",
        "US dollar / Yen",
    ),
    w(
        "USDCHF",
        "USDCHF=X",
        AssetClass::Fx,
        "CHF",
        "US dollar / Franc",
    ),
    w(
        "AUDUSD",
        "AUDUSD=X",
        AssetClass::Fx,
        "USD",
        "Australian / US dollar",
    ),
    w(
        "USDCAD",
        "USDCAD=X",
        AssetClass::Fx,
        "CAD",
        "US / Canadian dollar",
    ),
    w(
        "NZDUSD",
        "NZDUSD=X",
        AssetClass::Fx,
        "USD",
        "NZ / US dollar",
    ),
    w("EURGBP", "EURGBP=X", AssetClass::Fx, "GBP", "Euro / Pound"),
    w("EURJPY", "EURJPY=X", AssetClass::Fx, "JPY", "Euro / Yen"),
    w(
        "EURAUD",
        "EURAUD=X",
        AssetClass::Fx,
        "AUD",
        "Euro / Australian dollar",
    ),
    w("EURCHF", "EURCHF=X", AssetClass::Fx, "CHF", "Euro / Franc"),
    w("GBPJPY", "GBPJPY=X", AssetClass::Fx, "JPY", "Pound / Yen"),
    // Indices (CFDs at XTB).
    w("US500", "^GSPC", AssetClass::Index, "USD", "S&P 500"),
    w("US100", "^NDX", AssetClass::Index, "USD", "Nasdaq 100"),
    w("US30", "^DJI", AssetClass::Index, "USD", "Dow Jones"),
    w("DE40", "^GDAXI", AssetClass::Index, "EUR", "DAX"),
    w("UK100", "^FTSE", AssetClass::Index, "GBP", "FTSE 100"),
    w(
        "EU50",
        "^STOXX50E",
        AssetClass::Index,
        "EUR",
        "Euro Stoxx 50",
    ),
    w("JP225", "^N225", AssetClass::Index, "JPY", "Nikkei 225"),
    // Commodities (CFDs at XTB).
    w("GOLD", "GC=F", AssetClass::Commodity, "USD", "Gold"),
    w("SILVER", "SI=F", AssetClass::Commodity, "USD", "Silver"),
    w(
        "OIL.WTI",
        "CL=F",
        AssetClass::Commodity,
        "USD",
        "Crude oil WTI",
    ),
    w(
        "NATGAS",
        "NG=F",
        AssetClass::Commodity,
        "USD",
        "Natural gas",
    ),
    // Large US stocks.
    w("AAPL.US", "AAPL", AssetClass::Stock, "USD", "Apple"),
    w("MSFT.US", "MSFT", AssetClass::Stock, "USD", "Microsoft"),
    w("NVDA.US", "NVDA", AssetClass::Stock, "USD", "Nvidia"),
    w("AMZN.US", "AMZN", AssetClass::Stock, "USD", "Amazon"),
    w("GOOGL.US", "GOOGL", AssetClass::Stock, "USD", "Alphabet"),
    w("META.US", "META", AssetClass::Stock, "USD", "Meta"),
    w("TSLA.US", "TSLA", AssetClass::Stock, "USD", "Tesla"),
    w("AVGO.US", "AVGO", AssetClass::Stock, "USD", "Broadcom"),
    w("AMD.US", "AMD", AssetClass::Stock, "USD", "AMD"),
    w("JPM.US", "JPM", AssetClass::Stock, "USD", "JPMorgan"),
    w("XOM.US", "XOM", AssetClass::Stock, "USD", "Exxon Mobil"),
    w("NFLX.US", "NFLX", AssetClass::Stock, "USD", "Netflix"),
    // Large European stocks.
    w("ASML.NL", "ASML.AS", AssetClass::Stock, "EUR", "ASML"),
    w("SAP.DE", "SAP.DE", AssetClass::Stock, "EUR", "SAP"),
    w("MC.FR", "MC.PA", AssetClass::Stock, "EUR", "LVMH"),
    w("SIE.DE", "SIE.DE", AssetClass::Stock, "EUR", "Siemens"),
    // Broad ETFs.
    w("SPY.US", "SPY", AssetClass::Etf, "USD", "SPDR S&P 500"),
    w("QQQ.US", "QQQ", AssetClass::Etf, "USD", "Invesco QQQ"),
    w(
        "IWM.US",
        "IWM",
        AssetClass::Etf,
        "USD",
        "iShares Russell 2000",
    ),
    w(
        "TLT.US",
        "TLT",
        AssetClass::Etf,
        "USD",
        "iShares 20+ Year Treasury",
    ),
    w("GLD.US", "GLD", AssetClass::Etf, "USD", "SPDR Gold"),
];

/// The default watchlist as items.
pub fn default_watchlist() -> Vec<WatchItem> {
    DEFAULT_WATCHLIST
        .iter()
        .map(|(symbol, yahoo, class, currency, name)| WatchItem {
            symbol: (*symbol).into(),
            yahoo: (*yahoo).into(),
            class: *class,
            currency: (*currency).into(),
            name: (*name).into(),
        })
        .collect()
}

/// A change the owner made to the watchlist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WatchChange {
    /// Add (or replace) an entry.
    Add {
        /// The entry.
        item: WatchItem,
    },
    /// Remove an entry by XTB symbol.
    Remove {
        /// XTB symbol.
        symbol: String,
    },
}

/// The current watchlist: the defaults with `changes` applied in order.
pub fn apply(changes: &[WatchChange]) -> Vec<WatchItem> {
    let mut list = default_watchlist();
    for change in changes {
        match change {
            WatchChange::Add { item } => {
                list.retain(|w| !w.symbol.eq_ignore_ascii_case(&item.symbol));
                list.push(item.clone());
            }
            WatchChange::Remove { symbol } => {
                list.retain(|w| !w.symbol.eq_ignore_ascii_case(symbol));
            }
        }
    }
    list
}

/// Checks an entry the owner typed. The message is safe to show.
pub fn validate(item: &WatchItem) -> Result<(), String> {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 24
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-=^".contains(&b))
    };
    if !ok(&item.symbol) {
        return Err("The XTB symbol may use letters, digits and . _ - only.".into());
    }
    if !ok(&item.yahoo) {
        return Err("The Yahoo symbol may use letters, digits and . _ - = ^ only.".into());
    }
    if item.currency.len() != 3 || !item.currency.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err("The currency must be a three-letter code.".into());
    }
    if item.name.len() > 80 {
        return Err("The name is too long.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_unique() {
        let list = default_watchlist();
        for item in &list {
            assert!(validate(item).is_ok(), "{item:?}");
        }
        let mut symbols: Vec<&str> = list.iter().map(|w| w.symbol.as_str()).collect();
        symbols.sort();
        symbols.dedup();
        assert_eq!(symbols.len(), list.len());
    }

    #[test]
    fn changes_apply_in_order() {
        let item = WatchItem {
            symbol: "PLTR.US".into(),
            yahoo: "PLTR".into(),
            class: AssetClass::Stock,
            currency: "USD".into(),
            name: "Palantir".into(),
        };
        let list = apply(&[
            WatchChange::Add { item: item.clone() },
            WatchChange::Remove {
                symbol: "eurusd".into(),
            },
        ]);
        assert!(list.contains(&item));
        assert!(list.iter().all(|w| w.symbol != "EURUSD"));
    }
}
