//! Round 24: what the trade run feeds the setups beyond open/high/low/close: volume,
//! dividends going ex, listing region, earnings surprises, the market references, the
//! still-open cards and the returns behind the heat clusters.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;

use super::{CapitalService, ServiceError};
use crate::contracts::AccountId;
use crate::trade::heat::{CorrInputs, OpenCard};
use crate::trade::{AssetClass, Bar, Instrument, InstrumentContext, Region};
use crate::universe::WatchItem;

/// Sessions of daily returns kept per instrument for the heat clusters.
const CORRELATION_SESSIONS: usize = 80;

/// Most dividend histories fetched for the trade universe in one run.
const MAX_DIVIDEND_FETCH_PER_RUN: usize = 100;

/// The listing region from an XTB symbol: no suffix or `.US` is the US; the European
/// suffixes are Europe; anything else is other.
pub fn region_of(symbol: &str) -> Region {
    let upper = symbol.to_ascii_uppercase();
    let Some((_, suffix)) = upper.rsplit_once('.') else {
        return Region::Us;
    };
    match suffix {
        "US" => Region::Us,
        "DE" | "NL" | "FR" | "ES" | "IT" | "PT" | "UK" | "BE" | "IE" | "AT" | "FI" | "CH"
        | "SE" | "DK" | "NO" | "PL" | "CZ" => Region::Eu,
        // `BRK.B` and the like: a share class, not a venue.
        s if s.len() == 1 => Region::Us,
        _ => Region::Other,
    }
}

impl CapitalService {
    /// Bars for `key` known by `known_by` (open/high/low required), with volume and the
    /// dividends from `yahoo`'s stored history when its currency matches the prices'.
    pub(super) fn trade_bars(
        &self,
        key: &str,
        yahoo: Option<&str>,
        class: AssetClass,
        known_by: DateTime<Utc>,
        min_bars: usize,
    ) -> Result<Option<Vec<Bar>>, ServiceError> {
        let observations = self.store.prices(key, known_by)?;
        let currency = observations
            .last()
            .and_then(|o| o.currency.as_ref().map(|c| c.as_str().to_string()));
        let mut bars: Vec<Bar> = observations
            .iter()
            .filter_map(|o| {
                let ohl = o.ohl.as_ref()?;
                Some(Bar {
                    session: o.session,
                    open: ohl.open.to_f64()?,
                    high: ohl.high.to_f64()?,
                    low: ohl.low.to_f64()?,
                    close: o.close.to_f64()?,
                    volume: o
                        .volume
                        .and_then(|v| v.to_f64())
                        .filter(|v| v.is_finite() && *v > 0.0),
                    dividend: None,
                })
            })
            .filter(|b| b.open > 0.0 && b.high >= b.low && b.close > 0.0)
            .collect();
        if bars.len() < min_bars {
            return Ok(None);
        }
        if matches!(class, AssetClass::Stock | AssetClass::Etf)
            && let Some(yahoo) = yahoo
        {
            let dividends = self.dividends_by_ex_date(yahoo, currency.as_deref(), known_by)?;
            attach_dividends(&mut bars, &dividends);
        }
        Ok(Some(bars))
    }

    /// Per-share dividends by ex-date from the newest stored history known by `known_by`,
    /// only when the history's currency is the prices' currency (or either is unknown and
    /// neither is a minor unit such as GBX).
    fn dividends_by_ex_date(
        &self,
        yahoo: &str,
        price_currency: Option<&str>,
        known_by: DateTime<Utc>,
    ) -> Result<Vec<(NaiveDate, f64)>, ServiceError> {
        let Some((at, currency, history)) = self.store.dividend_history(yahoo)? else {
            return Ok(Vec::new());
        };
        if at > known_by {
            return Ok(Vec::new());
        }
        let compatible = match (currency.as_deref(), price_currency) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (Some(c), None) | (None, Some(c)) => !c.eq_ignore_ascii_case("GBX"),
            (None, None) => true,
        };
        if !compatible {
            return Ok(Vec::new());
        }
        Ok(history
            .into_iter()
            .filter(|p| p.per_share.is_finite() && p.per_share > 0.0)
            .map(|p| (p.ex_date, p.per_share))
            .collect())
    }

    /// The context of one instrument: region, earnings events known by `known_by` and the
    /// default withholding for its listing.
    pub(super) fn trade_context(
        &self,
        symbol: &str,
        yahoo: Option<&str>,
        class: AssetClass,
        known_by: DateTime<Utc>,
    ) -> Result<InstrumentContext, ServiceError> {
        let earnings_events = match (class, yahoo) {
            (AssetClass::Stock | AssetClass::Etf, Some(yahoo)) => {
                self.store.earnings_events(yahoo, known_by)?
            }
            _ => Vec::new(),
        };
        Ok(InstrumentContext {
            region: match class {
                AssetClass::Stock | AssetClass::Etf => region_of(symbol),
                _ => Region::Other,
            },
            earnings_events,
            withholding: match class {
                AssetClass::Stock | AssetClass::Etf => {
                    crate::dividends::default_withholding(symbol)
                }
                _ => 0.0,
            },
        })
    }

    /// The market references ([`crate::trade::regime::US_REFERENCE`] and
    /// [`crate::trade::regime::EU_REFERENCE`]) from the watchlist's stored bars.
    pub(super) fn trade_references(
        &self,
        watch: &[WatchItem],
        known_by: DateTime<Utc>,
    ) -> Result<HashMap<String, Vec<Bar>>, ServiceError> {
        let mut out = HashMap::new();
        for reference in [
            crate::trade::regime::US_REFERENCE,
            crate::trade::regime::EU_REFERENCE,
        ] {
            let Some(item) = watch
                .iter()
                .find(|w| w.yahoo.eq_ignore_ascii_case(reference))
            else {
                continue;
            };
            let key = super::trade_run::watch_key(&item.symbol);
            if let Some(bars) = self.trade_bars(&key, None, item.class, known_by, 1)? {
                out.insert(reference.to_string(), bars);
            }
        }
        Ok(out)
    }

    /// Published cards not graded yet, for portfolio heat.
    pub(super) fn open_trade_cards(
        &self,
        account: &AccountId,
    ) -> Result<Vec<OpenCard>, ServiceError> {
        Ok(self
            .store
            .trade_record(account)?
            .into_iter()
            .filter(|graded| graded.status == "open")
            .map(|graded| OpenCard {
                key: graded.card.key,
                side: graded.card.side,
                risk_pct: graded.card.risk_pct,
            })
            .collect())
    }

    /// Dividend histories for the trade universe's stocks and ETFs that are missing or a
    /// week old, at most [`MAX_DIVIDEND_FETCH_PER_RUN`] a run.
    pub(super) async fn refresh_trade_dividends(&self, yahoo: &[String]) {
        let now = Utc::now();
        let mut wanted = Vec::new();
        for symbol in yahoo {
            let fresh = self
                .store
                .dividend_history(symbol)
                .ok()
                .flatten()
                .is_some_and(|(at, ..)| {
                    (now - at).num_days() < super::dividends_run::HISTORY_MAX_AGE_DAYS
                });
            if !fresh {
                wanted.push(symbol.clone());
            }
            if wanted.len() >= MAX_DIVIDEND_FETCH_PER_RUN {
                break;
            }
        }
        if wanted.is_empty() {
            return;
        }
        if let Err(error) = self.fetch_dividend_histories(&wanted).await {
            tracing::warn!("trade dividend histories not refreshed: {error}");
        }
    }
}

/// Daily log returns over the last [`CORRELATION_SESSIONS`] sessions per instrument.
pub(super) fn correlation_inputs(universe: &[Instrument]) -> CorrInputs {
    let mut returns = HashMap::new();
    for instrument in universe.iter().filter(|i| !i.evidence_only) {
        let bars = &instrument.bars;
        let start = bars.len().saturating_sub(CORRELATION_SESSIONS + 1);
        let series: Vec<(NaiveDate, f64)> = bars[start..]
            .windows(2)
            .filter(|w| w[0].close > 0.0 && w[1].close > 0.0)
            .map(|w| (w[1].session, (w[1].close / w[0].close).ln()))
            .filter(|(_, r)| r.is_finite())
            .collect();
        returns.insert(instrument.key.clone(), series);
    }
    CorrInputs { returns }
}

/// Puts each dividend on the first bar on or after its ex-date (a holiday ex-date goes
/// to the next session), adding when two land on one bar. Dividends before the first
/// bar or after the last are dropped.
fn attach_dividends(bars: &mut [Bar], dividends: &[(NaiveDate, f64)]) {
    for (ex_date, amount) in dividends {
        let index = bars.partition_point(|bar| bar.session < *ex_date);
        if index == 0 && bars.first().is_none_or(|b| b.session != *ex_date) {
            continue;
        }
        if let Some(bar) = bars.get_mut(index) {
            bar.dividend = Some(bar.dividend.unwrap_or(0.0) + amount);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(day: u32) -> Bar {
        Bar::ohlc(
            NaiveDate::from_ymd_opt(2025, 3, day).unwrap(),
            10.0,
            11.0,
            9.0,
            10.0,
        )
    }

    #[test]
    fn region_reads_the_suffix() {
        assert_eq!(region_of("AAPL.US"), Region::Us);
        assert_eq!(region_of("AAPL"), Region::Us);
        assert_eq!(region_of("BRK.B"), Region::Us);
        assert_eq!(region_of("GALP.PT"), Region::Eu);
        assert_eq!(region_of("SHEL.UK"), Region::Eu);
        assert_eq!(region_of("7203.JP"), Region::Other);
    }

    #[test]
    fn dividends_land_on_the_ex_session_or_the_next() {
        let mut bars = vec![bar(3), bar(4), bar(6)];
        let d = |day| NaiveDate::from_ymd_opt(2025, 3, day).unwrap();
        attach_dividends(
            &mut bars,
            &[(d(1), 9.0), (d(4), 0.5), (d(5), 0.25), (d(9), 1.0)],
        );
        assert_eq!(
            bars.iter().map(|b| b.dividend).collect::<Vec<_>>(),
            vec![None, Some(0.5), Some(0.25)]
        );
    }
}
