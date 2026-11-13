//! `GET /markets`: the market board for the dashboard ([`crate::markets`]).

use super::{CapitalService, ServiceError, watch_key};
use crate::contracts::{AccountId, AsOf, InstrumentKind};
use crate::markets::{self, HeldSeries, MarketBoard, TileMeta};
use crate::trade::AssetClass;
use chrono::{NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;

/// A stored series: closes (oldest first) and its quote currency.
type Series = (Vec<(NaiveDate, f64)>, Option<String>);

fn class_name(class: AssetClass) -> String {
    serde_json::to_value(class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "other".into())
}

impl CapitalService {
    /// Recent price action for the holdings (owned shares and ETFs, not CFDs) and the
    /// watchlist, from stored closes only.
    pub fn markets(&self, account: &AccountId) -> Result<MarketBoard, ServiceError> {
        let now = Utc::now();
        let projection = self.portfolio(account, AsOf::now())?;
        let account_currency = self.account_currency(account)?;
        let watch = self.watchlist()?;

        // Closes (oldest first) and the quote currency of a stored series.
        let series_for = |key: &str| -> Result<Series, ServiceError> {
            let observations = self.store.prices(key, now)?;
            let currency = observations
                .last()
                .and_then(|o| o.currency.as_ref())
                .map(|c| c.as_str().to_string());
            let closes = observations
                .into_iter()
                .filter_map(|o| o.close.to_f64().map(|c| (o.session, c)))
                .collect();
            Ok((closes, currency))
        };

        let mut fx = super::FxCloses::new();
        for item in watch.iter().filter(|w| w.class == AssetClass::Fx) {
            if let Some((_, last)) = series_for(&watch_key(&item.symbol))?.0.last() {
                fx.insert(item.symbol.to_ascii_uppercase(), *last);
            }
        }

        let mut tiles = Vec::new();
        let mut without_prices = Vec::new();
        let mut held_series = Vec::new();
        let mut value_note = None;

        let mut positions: Vec<_> = projection
            .positions
            .iter()
            .filter(|p| p.instrument.kind != InstrumentKind::Cfd)
            .collect();
        positions.sort_by(|a, b| a.instrument.symbol.cmp(&b.instrument.symbol));
        for position in positions {
            let instrument = &position.instrument;
            let (closes, series_currency) = series_for(&instrument.key())?;
            let currency = series_currency
                .or_else(|| instrument.currency.as_ref().map(|c| c.as_str().to_string()));
            let meta = TileMeta {
                key: instrument.key(),
                symbol: instrument.symbol.clone(),
                name: instrument
                    .name
                    .clone()
                    .unwrap_or_else(|| instrument.symbol.clone()),
                class: match instrument.kind {
                    InstrumentKind::Etf => "etf".into(),
                    _ => "stock".into(),
                },
                currency: currency.clone(),
                held: true,
            };
            let rate = currency
                .as_deref()
                .and_then(|c| super::fx_rate(c, &account_currency, &fx));
            match (rate, position.quantity.to_f64(), closes.is_empty()) {
                (Some(rate), Some(units), false) => held_series.push((units, closes.clone(), rate)),
                (_, _, true) => {
                    value_note = Some(format!("{} has no stored closes.", instrument.symbol));
                }
                _ => {
                    value_note = Some(format!(
                        "No exchange rate into {account_currency} for {}.",
                        instrument.symbol
                    ));
                }
            }
            match markets::tile(meta, &closes) {
                Some(tile) => tiles.push(tile),
                None => without_prices.push(instrument.symbol.clone()),
            }
        }

        for item in &watch {
            if tiles
                .iter()
                .any(|t| t.meta.symbol.eq_ignore_ascii_case(&item.symbol))
            {
                continue;
            }
            let meta = TileMeta {
                key: watch_key(&item.symbol),
                symbol: item.symbol.clone(),
                name: item.name.clone(),
                class: class_name(item.class),
                currency: Some(item.currency.clone()),
                held: false,
            };
            match markets::tile(meta, &series_for(&watch_key(&item.symbol))?.0) {
                Some(tile) => tiles.push(tile),
                None => without_prices.push(item.symbol.clone()),
            }
        }

        let holdings_value = if value_note.is_none() {
            let series: Vec<HeldSeries<'_>> = held_series
                .iter()
                .map(|(units, closes, fx)| HeldSeries {
                    units: *units,
                    closes,
                    fx: *fx,
                })
                .collect();
            markets::holdings_value(&series, markets::SPARK_SESSIONS)
        } else {
            Vec::new()
        };
        if holdings_value.is_empty() && value_note.is_none() {
            value_note = Some("No holdings with prices yet.".into());
        }

        Ok(MarketBoard {
            account_currency,
            session: tiles.iter().map(|t| t.session).max(),
            breadth: markets::breadth(&tiles),
            tiles,
            without_prices,
            holdings_value,
            holdings_value_note: value_note,
        })
    }
}
