//! The signal universe and `GET /signals/board` ([`crate::board`]).

use super::{CapitalService, ServiceError, watch_key};
use crate::board::{BoardMeta, OutlookRecord, RowInput, SetupView, SignalBoard};
use crate::contracts::{AccountId, DecisionTime, InputProvenance, InstrumentKind};
use crate::replay::{MembershipInputManifest, hash_universe_pairs};
use crate::signals::SignalId;
use crate::trade::AssetClass;
use rust_decimal::prelude::ToPrimitive;

impl CapitalService {
    /// Signal universe as known at `decision`, along with its membership manifest entries.
    pub(super) fn signal_universe_as_of(
        &self,
        account: &AccountId,
        decision: DecisionTime,
    ) -> Result<(Vec<BoardMeta>, Vec<MembershipInputManifest>), ServiceError> {
        let projection = self.portfolio(account, decision.as_of())?;
        let mut out: Vec<BoardMeta> = Vec::new();
        let mut portfolio_pairs: Vec<(String, String)> = Vec::new();
        for position in &projection.positions {
            let instrument = &position.instrument;
            if instrument.kind == InstrumentKind::Cfd
                || out
                    .iter()
                    .any(|m| m.symbol.eq_ignore_ascii_case(&instrument.symbol))
            {
                continue;
            }
            portfolio_pairs.push((instrument.key(), instrument.symbol.clone()));
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
        let mut watch_pairs: Vec<(String, String)> = Vec::new();
        for item in self.watchlist_as_of(decision)? {
            if !matches!(item.class, AssetClass::Stock | AssetClass::Etf)
                || out
                    .iter()
                    .any(|m| m.symbol.eq_ignore_ascii_case(&item.symbol))
            {
                continue;
            }
            let key = watch_key(&item.symbol);
            watch_pairs.push((key.clone(), item.symbol.clone()));
            out.push(BoardMeta {
                key,
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
        let manifests = vec![
            MembershipInputManifest {
                source: "portfolio".into(),
                provenance: InputProvenance::Observed,
                snapshot_at: Some(decision.at),
                snapshot_sha256: hash_universe_pairs(&portfolio_pairs),
            },
            MembershipInputManifest {
                source: "watchlist".into(),
                provenance: InputProvenance::Observed,
                snapshot_at: Some(decision.at),
                snapshot_sha256: hash_universe_pairs(&watch_pairs),
            },
        ];
        Ok((out, manifests))
    }

    /// `(key, symbol)` pairs of [`Self::signal_universe`], sorted by key.
    pub(super) fn universe_pairs(
        &self,
        account: &AccountId,
    ) -> Result<Vec<(String, String)>, ServiceError> {
        self.universe_pairs_as_of(account, DecisionTime::now())
    }

    /// `(key, symbol)` pairs of [`Self::signal_universe_as_of`], sorted by key.
    pub(super) fn universe_pairs_as_of(
        &self,
        account: &AccountId,
        decision: DecisionTime,
    ) -> Result<Vec<(String, String)>, ServiceError> {
        let mut pairs: Vec<(String, String)> = self
            .signal_universe_as_of(account, decision)?
            .0
            .into_iter()
            .map(|m| (m.key, m.symbol))
            .collect();
        pairs.sort();
        Ok(pairs)
    }

    /// Every instrument's signals, outlook and setup, side by side. Cached until prices,
    /// model values, the universe, the trade run or the day change.
    pub fn signal_board(&self, account: &AccountId) -> Result<SignalBoard, ServiceError> {
        let decision = DecisionTime::now();
        let report = self
            .signals_as_of(
                account,
                decision,
                crate::contracts::VintagePolicy::ObservedOnly,
            )?
            .value;
        let (universe, _) = self.signal_universe_as_of(account, decision)?;
        let latest_run = self.store.latest_trade_run(account)?;
        let paths: u64 = self
            .store
            .outlook_path_counts()?
            .iter()
            .map(|(.., n)| u64::from(*n))
            .sum();
        let model_values: u64 = self
            .store
            .signal_value_coverage()?
            .iter()
            .map(|c| u64::from(c.values))
            .sum();
        let version = format!(
            "{}|{}|{}|{}|{}|{:?}|{}|{}",
            account.as_str(),
            self.store.price_version()?,
            paths,
            model_values,
            report
                .combined_ic
                .iter()
                .map(|(h, ic)| format!("{h}:{ic}"))
                .collect::<Vec<_>>()
                .join(","),
            latest_run.as_ref().map(|(at, _)| *at),
            decision.session,
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
        let data = self.closes_for(&pairs, decision.at)?;
        let day = latest_run.as_ref().map(|(_, day)| day);
        // The latest model value per (signal, instrument), with its session.
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        let mut model_latest: std::collections::BTreeMap<
            (SignalId, String),
            (chrono::NaiveDate, f64),
        > = Default::default();
        for scores in self.external_scores_as_of(&keys, decision)?.0 {
            for (key, session, value) in scores.values {
                let slot = model_latest
                    .entry((scores.signal, key))
                    .or_insert((session, value));
                if session > slot.0 {
                    *slot = (session, value);
                }
            }
        }
        let mut inputs = Vec::with_capacity(universe.len());
        for (meta, (key, symbol, closes)) in universe.into_iter().zip(data.iter()) {
            let series = crate::forecast::CloseSeries {
                instrument_key: key,
                symbol,
                closes,
            };
            let mut raw: Vec<(SignalId, f64)> = SignalId::PRICE
                .iter()
                .filter_map(|s| crate::signals::price_signal(&series, *s).map(|v| (*s, v)))
                .collect();
            // Model values count only when recent against the instrument's last close.
            if let Some((last, _)) = closes.last() {
                for ((signal, k), (session, value)) in &model_latest {
                    if k == key
                        && (*last - *session).num_days() <= crate::signals::MAX_EXTERNAL_AGE_DAYS
                    {
                        raw.push((*signal, *value));
                    }
                }
            }
            let mut outlook = Vec::new();
            if !closes.is_empty() {
                let external = self.external_paths_as_of(key, decision)?.0;
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
                        candidate: false,
                    });
                }
                if let Some(card) = day
                    .candidates
                    .iter()
                    .find(|c| c.symbol.eq_ignore_ascii_case(&meta.symbol))
                {
                    return Some(SetupView {
                        setup: card.setup,
                        side: card.side,
                        published: false,
                        candidate: true,
                    });
                }
                day.suppressed
                    .iter()
                    .find(|s| s.symbol.eq_ignore_ascii_case(&meta.symbol))
                    .map(|s| SetupView {
                        setup: s.setup,
                        side: s.side,
                        published: false,
                        candidate: false,
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
