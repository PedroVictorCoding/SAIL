//! The signal universe and `GET /signals/board` ([`crate::board`]).

use super::{CapitalService, ServiceError, watch_key};
use crate::board::{BoardMeta, OutlookRecord, RowInput, SetupView, SignalBoard};
use crate::contracts::{AccountId, AsOf, InstrumentKind};
use crate::signals::SignalId;
use crate::trade::AssetClass;
use chrono::Utc;
use rust_decimal::prelude::ToPrimitive;

impl CapitalService {
    /// The instruments signals are ranked across: owned holdings (shares and ETFs, not
    /// CFDs), then the watchlist's stocks and ETFs that aren't held. Ranking against a
    /// few dozen names instead of only the holdings gives each signal enough
    /// cross-sections to be tested. FX, indices and commodities are left out: ranking a
    /// currency against a share says nothing.
    pub(super) fn signal_universe(
        &self,
        account: &AccountId,
    ) -> Result<Vec<BoardMeta>, ServiceError> {
        let projection = self.portfolio(account, AsOf::now())?;
        let mut out: Vec<BoardMeta> = Vec::new();
        for position in &projection.positions {
            let instrument = &position.instrument;
            if instrument.kind == InstrumentKind::Cfd
                || out
                    .iter()
                    .any(|m| m.symbol.eq_ignore_ascii_case(&instrument.symbol))
            {
                continue;
            }
            out.push(BoardMeta {
                key: instrument.key(),
                symbol: instrument.symbol.clone(),
                name: instrument
                    .name
                    .clone()
                    .unwrap_or_else(|| instrument.symbol.clone()),
                class: if instrument.kind == InstrumentKind::Etf {
                    "etf".into()
                } else {
                    "stock".into()
                },
                held: true,
            });
        }
        for item in self.watchlist()? {
            if !matches!(item.class, AssetClass::Stock | AssetClass::Etf)
                || out
                    .iter()
                    .any(|m| m.symbol.eq_ignore_ascii_case(&item.symbol))
            {
                continue;
            }
            out.push(BoardMeta {
                key: watch_key(&item.symbol),
                symbol: item.symbol.clone(),
                name: item.name.clone(),
                class: if item.class == AssetClass::Etf {
                    "etf".into()
                } else {
                    "stock".into()
                },
                held: false,
            });
        }
        Ok(out)
    }

    /// `(key, symbol)` pairs of [`Self::signal_universe`], sorted by key.
    pub(super) fn universe_pairs(
        &self,
        account: &AccountId,
    ) -> Result<Vec<(String, String)>, ServiceError> {
        let mut pairs: Vec<(String, String)> = self
            .signal_universe(account)?
            .into_iter()
            .map(|m| (m.key, m.symbol))
            .collect();
        pairs.sort();
        Ok(pairs)
    }

    /// Every instrument's signals, outlook and setup, side by side. Cached until prices,
    /// model values, the universe, the trade run or the day change.
    pub fn signal_board(&self, account: &AccountId) -> Result<SignalBoard, ServiceError> {
        let report = self.signals(account)?;
        let universe = self.signal_universe(account)?;
        let latest_run = self.store.latest_trade_run(account)?;
        let paths: u64 = self
            .store
            .outlook_path_counts()?
            .iter()
            .map(|(.., n)| u64::from(*n))
            .sum();
        let version = format!(
            "{}|{}|{}|{}|{:?}|{}|{}",
            account.as_str(),
            self.store.price_version()?,
            paths,
            report
                .combined_ic
                .iter()
                .map(|(h, ic)| format!("{h}:{ic}"))
                .collect::<Vec<_>>()
                .join(","),
            latest_run.as_ref().map(|(at, _)| *at),
            Utc::now().date_naive(),
            universe
                .iter()
                .map(|m| m.key.as_str())
                .collect::<Vec<_>>()
                .join("|")
        );
        if let Ok(cache) = self.board_cache.lock()
            && let Some((cached, board)) = cache.as_ref()
            && *cached == version
        {
            return Ok(board.clone());
        }

        let pairs: Vec<(String, String)> = universe
            .iter()
            .map(|m| (m.key.clone(), m.symbol.clone()))
            .collect();
        let data = self.closes_for(&pairs, Utc::now())?;
        let day = latest_run.as_ref().map(|(_, day)| day);
        let mut inputs = Vec::with_capacity(universe.len());
        for (meta, (key, symbol, closes)) in universe.into_iter().zip(data.iter()) {
            let series = crate::forecast::CloseSeries {
                instrument_key: key,
                symbol,
                closes,
            };
            let raw: Vec<(SignalId, f64)> = SignalId::PRICE
                .iter()
                .filter_map(|s| crate::signals::price_signal(&series, *s).map(|v| (*s, v)))
                .collect();
            let mut outlook = Vec::new();
            if !closes.is_empty() {
                let external = self.external_paths(key)?;
                for (signal_h, outlook_h) in crate::board::BOARD_HORIZONS {
                    let alpha = report
                        .latest
                        .iter()
                        .find(|c| c.instrument_key == *key && c.horizon_sessions == signal_h)
                        .map(|c| c.alpha)
                        .unwrap_or(0.0);
                    let instrument = crate::outlook::Instrument {
                        series: series.clone(),
                        external: external.clone(),
                        tilt: vec![(outlook_h, alpha)],
                        tilt_signals: Vec::new(),
                    };
                    let o = crate::outlook::outlook(&instrument, outlook_h, None);
                    outlook.extend(crate::board::summarize_outlook(&o));
                }
            }
            let setup = day.and_then(|day| {
                if let Some(card) = day
                    .cards
                    .iter()
                    .find(|c| c.symbol.eq_ignore_ascii_case(&meta.symbol))
                {
                    return Some(SetupView {
                        setup: card.setup,
                        side: card.side,
                        published: true,
                    });
                }
                day.suppressed
                    .iter()
                    .find(|(s, ..)| s.eq_ignore_ascii_case(&meta.symbol))
                    .map(|(_, setup, side)| SetupView {
                        setup: *setup,
                        side: *side,
                        published: false,
                    })
            });
            inputs.push(RowInput {
                meta,
                closes: closes
                    .iter()
                    .filter_map(|(d, c)| Some((*d, c.to_f64()?)))
                    .collect(),
                raw,
                outlook,
                setup,
            });
        }

        let mut outlook_record = Vec::new();
        for (_, outlook_h) in crate::board::BOARD_HORIZONS {
            if let Ok(r) = self.track_record(account, outlook_h)
                && r.replays > 0
            {
                outlook_record.push(OutlookRecord {
                    horizon_sessions: outlook_h,
                    replays: r.replays,
                    direction_hit_rate: r.direction_hit_rate,
                    inside_90: r.inside_90,
                    skill_vs_no_change: r.skill_vs_no_change,
                });
            }
        }
        let board = crate::board::build(inputs, &report, outlook_record, day.map(|d| d.session));
        if let Ok(mut cache) = self.board_cache.lock() {
            *cache = Some((version, board.clone()));
        }
        Ok(board)
    }
}
