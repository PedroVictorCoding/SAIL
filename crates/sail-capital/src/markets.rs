//! The market board: recent price action for the holdings and the watchlist, as the
//! dashboard shows it (ticker strip, price cards, index tiles, breadth gauge, market table).
//!
//! Everything here is computed from closes already in the store. Nothing is fetched and
//! nothing is forecast: these are descriptions of the past, labelled as such.
//!
//! - **Changes** are simple returns of the last close against the close 1, 5 and 21
//!   sessions earlier (`change_1d`, `change_1w`, `change_1m`), as fractions (0.012 = 1.2%).
//!   A change is `None` when the series is too short.
//! - **Spark** is the last [`SPARK_SESSIONS`] closes, oldest first.
//! - **Above 200-day:** the last close against the mean of the last 200 closes; `None` with
//!   fewer than 200.
//! - **Breadth** counts the tiles that have a 200-day answer and how many are above. It is
//!   the honest stand-in for a "sentiment" gauge: a measured share, not a mood score.
//! - **Holdings value** is today's units times each session's close times today's FX rate,
//!   over the last [`SPARK_SESSIONS`] sessions. It shows how the current holdings would have
//!   moved, not the account's history (deposits and trades are ignored), and it is empty
//!   unless every holding has a close and an FX rate.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Closes in a spark line.
pub const SPARK_SESSIONS: usize = 30;

/// Sessions behind the weekly and monthly changes.
pub const WEEK_SESSIONS: usize = 5;
/// Sessions behind the monthly change.
pub const MONTH_SESSIONS: usize = 21;
/// Sessions in the long trend average.
pub const TREND_SESSIONS: usize = 200;

/// What a tile is, before its prices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TileMeta {
    /// Store key of the price series.
    pub key: String,
    /// XTB symbol.
    pub symbol: String,
    /// Display name.
    pub name: String,
    /// `stock`, `etf`, `fx`, `index`, `commodity`, …
    pub class: String,
    /// Quote currency, when known.
    pub currency: Option<String>,
    /// The owner holds it.
    pub held: bool,
}

/// One instrument's recent price action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarketTile {
    /// What it is.
    #[serde(flatten)]
    pub meta: TileMeta,
    /// Session of the last close.
    pub session: NaiveDate,
    /// Last close.
    pub close: f64,
    /// Against one session earlier.
    pub change_1d: Option<f64>,
    /// Against [`WEEK_SESSIONS`] sessions earlier.
    pub change_1w: Option<f64>,
    /// Against [`MONTH_SESSIONS`] sessions earlier.
    pub change_1m: Option<f64>,
    /// The last [`SPARK_SESSIONS`] closes, oldest first.
    pub spark: Vec<f64>,
    /// Last close above the [`TREND_SESSIONS`]-close mean.
    pub above_200d: Option<bool>,
}

/// Share of instruments above their long average.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Breadth {
    /// Instruments above.
    pub above: u32,
    /// Instruments with enough history to tell.
    pub total: u32,
}

/// `GET /markets`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarketBoard {
    /// The account currency the holdings value is in.
    pub account_currency: String,
    /// Latest session across tiles.
    pub session: Option<NaiveDate>,
    /// Holdings first (by symbol), then the watchlist (by its order).
    pub tiles: Vec<MarketTile>,
    /// Instruments asked for that have no stored closes.
    pub without_prices: Vec<String>,
    /// Breadth over every tile.
    pub breadth: Breadth,
    /// Today's holdings valued at each recent session (see the module doc).
    pub holdings_value: Vec<(NaiveDate, f64)>,
    /// Why `holdings_value` is empty, when it is.
    pub holdings_value_note: Option<String>,
}

fn change(closes: &[(NaiveDate, f64)], back: usize) -> Option<f64> {
    let n = closes.len();
    if n <= back {
        return None;
    }
    let then = closes[n - 1 - back].1;
    (then > 0.0).then(|| closes[n - 1].1 / then - 1.0)
}

/// The tile for one series (closes oldest first), or `None` without a positive close.
pub fn tile(meta: TileMeta, closes: &[(NaiveDate, f64)]) -> Option<MarketTile> {
    let closes: Vec<(NaiveDate, f64)> = closes
        .iter()
        .copied()
        .filter(|(_, c)| c.is_finite() && *c > 0.0)
        .collect();
    let &(session, close) = closes.last()?;
    let above_200d = (closes.len() >= TREND_SESSIONS).then(|| {
        let tail = &closes[closes.len() - TREND_SESSIONS..];
        let mean = tail.iter().map(|(_, c)| c).sum::<f64>() / TREND_SESSIONS as f64;
        close > mean
    });
    Some(MarketTile {
        meta,
        session,
        close,
        change_1d: change(&closes, 1),
        change_1w: change(&closes, WEEK_SESSIONS),
        change_1m: change(&closes, MONTH_SESSIONS),
        spark: closes
            .iter()
            .skip(closes.len().saturating_sub(SPARK_SESSIONS))
            .map(|(_, c)| *c)
            .collect(),
        above_200d,
    })
}

/// Breadth over tiles.
pub fn breadth(tiles: &[MarketTile]) -> Breadth {
    let known: Vec<bool> = tiles.iter().filter_map(|t| t.above_200d).collect();
    Breadth {
        above: known.iter().filter(|a| **a).count() as u32,
        total: known.len() as u32,
    }
}

/// One holding for [`holdings_value`]: units, closes (oldest first) and the FX rate into
/// the account currency.
pub struct HeldSeries<'a> {
    /// Units held now.
    pub units: f64,
    /// Its closes.
    pub closes: &'a [(NaiveDate, f64)],
    /// Account currency per unit of quote currency.
    pub fx: f64,
}

/// Today's holdings valued at each of the last `sessions` dates on which any holding
/// traded, carrying each holding's last known close forward. Dates before every holding
/// has a close are skipped.
pub fn holdings_value(held: &[HeldSeries<'_>], sessions: usize) -> Vec<(NaiveDate, f64)> {
    if held.is_empty() || held.iter().any(|h| h.closes.is_empty()) {
        return Vec::new();
    }
    let start = held
        .iter()
        .map(|h| h.closes[0].0)
        .max()
        .unwrap_or(NaiveDate::MIN);
    let mut dates: Vec<NaiveDate> = held
        .iter()
        .flat_map(|h| h.closes.iter().map(|(d, _)| *d))
        .filter(|d| *d >= start)
        .collect();
    dates.sort_unstable();
    dates.dedup();
    let dates = &dates[dates.len().saturating_sub(sessions)..];
    dates
        .iter()
        .map(|date| {
            let value = held
                .iter()
                .map(|h| {
                    let at = h.closes.partition_point(|(d, _)| d <= date);
                    h.units * h.closes[at - 1].1 * h.fx
                })
                .sum();
            (*date, value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: i64) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Duration::days(n)
    }

    fn meta() -> TileMeta {
        TileMeta {
            key: "k".into(),
            symbol: "S".into(),
            name: "S".into(),
            class: "stock".into(),
            currency: Some("USD".into()),
            held: true,
        }
    }

    #[test]
    fn changes_spark_and_trend() {
        let closes: Vec<_> = (0..250).map(|i| (day(i), 100.0 + i as f64)).collect();
        let t = tile(meta(), &closes).unwrap();
        assert_eq!(t.close, 349.0);
        assert!((t.change_1d.unwrap() - (349.0 / 348.0 - 1.0)).abs() < 1e-12);
        assert!((t.change_1w.unwrap() - (349.0 / 344.0 - 1.0)).abs() < 1e-12);
        assert!((t.change_1m.unwrap() - (349.0 / 328.0 - 1.0)).abs() < 1e-12);
        assert_eq!(t.spark.len(), SPARK_SESSIONS);
        assert_eq!(*t.spark.last().unwrap(), 349.0);
        assert_eq!(t.above_200d, Some(true));
    }

    #[test]
    fn short_series_leaves_gaps() {
        let t = tile(meta(), &[(day(0), 10.0), (day(1), 11.0)]).unwrap();
        assert!(t.change_1d.is_some());
        assert_eq!(t.change_1w, None);
        assert_eq!(t.above_200d, None);
        assert!(tile(meta(), &[]).is_none());
        assert!(tile(meta(), &[(day(0), 0.0)]).is_none());
    }

    #[test]
    fn breadth_counts_known_only() {
        let mut a = tile(meta(), &[(day(0), 1.0)]).unwrap();
        let mut b = a.clone();
        let c = a.clone();
        a.above_200d = Some(true);
        b.above_200d = Some(false);
        assert_eq!(breadth(&[a, b, c]), Breadth { above: 1, total: 2 });
    }

    #[test]
    fn holdings_value_carries_forward_and_converts() {
        let a = [(day(0), 10.0), (day(1), 11.0), (day(3), 12.0)];
        let b = [(day(1), 100.0), (day(2), 110.0)];
        let held = [
            HeldSeries {
                units: 2.0,
                closes: &a,
                fx: 1.0,
            },
            HeldSeries {
                units: 1.0,
                closes: &b,
                fx: 0.5,
            },
        ];
        let v = holdings_value(&held, 10);
        assert_eq!(
            v,
            vec![
                (day(1), 22.0 + 50.0),
                (day(2), 22.0 + 55.0),
                (day(3), 24.0 + 55.0)
            ]
        );
        assert_eq!(holdings_value(&held, 1), vec![(day(3), 79.0)]);
        assert!(holdings_value(&[], 5).is_empty());
    }
}
