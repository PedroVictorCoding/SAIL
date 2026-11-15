//! Round 19: `GET /signals/trade/health` and the research universe (Claude's slice).
use super::{CapitalService, ServiceError, trade_run::MIN_OHL_BARS};
use crate::contracts::{AccountId, AsOf, DecisionTime, InstrumentKind, VintagePolicy};
use crate::trade::quality::{Gap, gaps, usable};
use crate::trade::{AssetClass, Bar, Instrument};
use chrono::{DateTime, Datelike, Days, NaiveDate, Utc, Weekday};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `GET /signals/trade/health`: is the data behind the signals sound and fresh?
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TradeHealth {
    /// The latest run, when any.
    pub run_at: Option<DateTime<Utc>>,
    /// Its counts and timing.
    pub run_stats: Option<crate::trade::RunStats>,
    /// Why the last run failed, if it did.
    pub last_error: Option<String>,
    /// One row per watchlist and held instrument, worst first.
    pub instruments: Vec<InstrumentHealth>,
    /// The research universe, when it is on.
    pub research: Option<ResearchCoverage>,
    /// Plain notes, each once.
    pub notes: Vec<String>,
}

/// One instrument's data health.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstrumentHealth {
    /// Instrument key.
    pub key: String,
    /// XTB symbol.
    pub symbol: String,
    /// Latest session with a close, if any.
    pub last_session: Option<NaiveDate>,
    /// Weekdays between that session and today (0 = fresh).
    pub stale_weekdays: u32,
    /// Bars with open, high and low.
    pub bars: u32,
    /// Enough bars to be replayed (`MIN_OHL_BARS`).
    pub enough_bars: bool,
    /// Flagged jumps ([`crate::trade::quality::gaps`]).
    pub gaps: Vec<Gap>,
    /// Left out of the evidence because of a likely unadjusted split.
    pub excluded: bool,
    /// Stocks: whether an earnings date is known.
    pub earnings_known: Option<bool>,
    /// The last price fetch failed.
    pub fetch_failed: bool,
}

/// How much of the research universe has prices.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResearchCoverage {
    /// Indexes on, e.g. `["sp500", "stoxx50"]`.
    pub indexes: Vec<String>,
    /// When membership was last fetched.
    pub fetched_at: Option<DateTime<Utc>>,
    /// Members, current and former.
    pub members: u32,
    /// Former members (left the index).
    pub former: u32,
    /// Members with enough bars to replay.
    pub with_prices: u32,
    /// Former members with enough bars (survivorship coverage).
    pub former_with_prices: u32,
    /// Excluded for a likely split.
    pub excluded: u32,
}

impl CapitalService {
    /// Claude, round 19.
    pub fn trade_health(&self, account: &AccountId) -> Result<TradeHealth, ServiceError> {
        let now = Utc::now();
        let trade = self.trade_view(account)?;
        let run = self.store.latest_trade_run(account)?;
        let mut notes = Vec::new();

        let watch = self.watchlist()?;
        let held = self.held_instruments(account)?;
        let portfolio = self.portfolio(account, AsOf::now())?;
        let mut classes = BTreeMap::new();
        for position in &portfolio.positions {
            let class = match position.instrument.kind {
                InstrumentKind::Etf => AssetClass::Etf,
                _ => AssetClass::Stock,
            };
            classes.insert(position.instrument.key(), class);
        }
        let mut entries: BTreeMap<String, (String, AssetClass, Option<String>)> = BTreeMap::new();
        for item in watch {
            entries.insert(
                super::watch_key(&item.symbol),
                (item.symbol, item.class, Some(item.yahoo)),
            );
        }
        for (key, symbol) in held {
            let class = classes.get(&key).copied().unwrap_or(AssetClass::Stock);
            entries.insert(
                key,
                (symbol.clone(), class, crate::market::yahoo_symbol(&symbol)),
            );
        }

        let mut instruments = Vec::with_capacity(entries.len());
        let mut excluded = false;
        for (key, (symbol, class, yahoo)) in entries {
            let observations = self.store.prices(&key, now)?;
            let last_session = observations.last().map(|observation| observation.session);
            let bars: Vec<Bar> = observations
                .iter()
                .filter_map(|observation| {
                    let ohl = observation.ohl.as_ref()?;
                    Some(Bar::ohlc(
                        observation.session,
                        ohl.open.to_f64()?,
                        ohl.high.to_f64()?,
                        ohl.low.to_f64()?,
                        observation.close.to_f64()?,
                    ))
                })
                .filter(|bar| bar.open > 0.0 && bar.high >= bar.low && bar.close > 0.0)
                .collect();
            let flagged = gaps(&bars);
            let left_out = !usable(&bars);
            excluded |= left_out;
            let earnings_known = matches!(class, AssetClass::Stock | AssetClass::Etf).then(|| {
                yahoo.as_deref().is_some_and(|symbol| {
                    self.store
                        .earnings(symbol)
                        .ok()
                        .flatten()
                        .is_some_and(|(_, dates)| !dates.is_empty())
                })
            });
            instruments.push(InstrumentHealth {
                key,
                symbol,
                last_session,
                stale_weekdays: last_session
                    .map_or(0, |session| weekdays_since(session, now.date_naive())),
                bars: bars.len() as u32,
                enough_bars: bars.len() >= MIN_OHL_BARS,
                gaps: flagged,
                excluded: left_out,
                earnings_known,
                fetch_failed: self.prices_configured && last_session.is_none(),
            });
        }
        instruments.sort_by_key(|instrument| {
            (
                !instrument.excluded,
                !instrument.fetch_failed,
                instrument.stale_weekdays <= 2,
                instrument.enough_bars,
                instrument.earnings_known != Some(false),
                instrument.symbol.clone(),
            )
        });

        if !self.prices_configured {
            notes.push("Price fetching is off; health can only show prices already stored.".into());
        }
        if instruments.is_empty() {
            notes.push("No watchlist or held instruments to check.".into());
        }
        if excluded {
            notes.push("A named split takes an instrument out of the evidence.".into());
        }
        notes.push("Weekdays are calendar weekdays, not exchange sessions.".into());

        let research = self.research_coverage(now)?;
        if research.is_none() {
            notes.push("Research universe is off (set SAIL_TRADE_RESEARCH=sp500,stoxx50).".into());
        } else if research
            .as_ref()
            .is_some_and(|coverage| coverage.former == 0)
        {
            notes.push(
                "No former index members are stored, so some survivorship bias remains.".into(),
            );
        }

        Ok(TradeHealth {
            run_at: run.as_ref().map(|(at, _)| *at),
            run_stats: run.and_then(|(_, day)| day.run_stats),
            last_error: trade.last_error,
            instruments,
            research,
            notes,
        })
    }

    /// Refreshes membership snapshots when stale, then fetches a bounded batch of missing
    /// or stale research prices. Errors are logged, never fatal to the signal run.
    pub(super) async fn refresh_research(&self, now: DateTime<Utc>) {
        let Some(indexes) = research_indexes() else {
            return;
        };
        let stale = indexes
            .iter()
            .filter(|index| {
                self.store
                    .research_members(index)
                    .ok()
                    .flatten()
                    .is_none_or(|(at, _)| now - at > chrono::Duration::days(RESEARCH_MAX_AGE_DAYS))
            })
            .cloned()
            .collect::<Vec<_>>();
        if !stale.is_empty()
            && let Err(error) = self.fetch_membership(&stale).await
        {
            tracing::warn!(error = %error, "research membership fetch failed");
        }

        let mut members: BTreeMap<String, crate::universe::ResearchMember> = BTreeMap::new();
        for index in indexes {
            if let Ok(Some((_, stored))) = self.store.research_members(&index) {
                for member in stored {
                    members.insert(member.yahoo.clone(), member);
                }
            }
        }
        let mut fetched = 0usize;
        for member in members.values() {
            if fetched >= RESEARCH_MAX_FETCHES {
                break;
            }
            let key = research_key(&member.yahoo);
            let fresh = self
                .store
                .prices(&key, now)
                .ok()
                .and_then(|observations| observations.last().map(|o| o.session))
                .is_some_and(|session| weekdays_since(session, now.date_naive()) <= 3);
            if fresh {
                continue;
            }
            match self.prices.daily_as(&member.yahoo, &member.yahoo).await {
                Ok(series) => {
                    let store = self.store.clone();
                    if let Err(error) = tokio::task::spawn_blocking(move || {
                        store.record_prices(&key, &series, Utc::now())
                    })
                    .await
                    .unwrap_or_else(|_| Err(crate::store::StoreError::Poisoned))
                    {
                        tracing::warn!(error = %error, "research price store failed");
                    }
                    fetched += 1;
                }
                Err(crate::market::PriceSourceError::Refused(_))
                | Err(crate::market::PriceSourceError::Blocked) => break,
                Err(error) => {
                    tracing::warn!(symbol = %member.yahoo, error = %error, "research price fetch failed");
                    fetched += 1;
                }
            }
        }
    }

    async fn fetch_membership(&self, indexes: &[String]) -> Result<(), ServiceError> {
        let python = std::env::var("SAIL_YFINANCE_PYTHON")
            .unwrap_or_else(|_| crate::market::yahoo::DEFAULT_PYTHON.to_string());
        let script = std::env::var("SAIL_RESEARCH_SCRIPT")
            .unwrap_or_else(|_| "scripts/prices/universe.py".to_string());
        let mut command = tokio::process::Command::new(&python);
        command
            .arg("-I")
            .arg(&script)
            .args(indexes)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        for var in [
            "PATH",
            "HOME",
            "LANG",
            "SSL_CERT_FILE",
            "HTTPS_PROXY",
            "https_proxy",
        ] {
            if let Ok(value) = std::env::var(var) {
                command.env(var, value);
            }
        }
        command.env("XDG_CACHE_HOME", crate::market::yahoo::DEFAULT_CACHE);
        let Ok(Ok(output)) =
            tokio::time::timeout(std::time::Duration::from_secs(180), command.output()).await
        else {
            return Err(ServiceError::BadRequest(
                "The research membership helper did not finish.".into(),
            ));
        };
        if !output.status.success() {
            return Err(ServiceError::BadRequest(
                "The research membership helper failed.".into(),
            ));
        }
        #[derive(Deserialize)]
        struct Out {
            #[serde(default)]
            indexes: BTreeMap<String, Vec<crate::universe::ResearchMember>>,
            #[serde(default)]
            notes: Vec<String>,
        }
        let out: Out = serde_json::from_slice(&output.stdout).map_err(|error| {
            ServiceError::BadRequest(format!("Research membership output unreadable: {error}"))
        })?;
        for (index, members) in out.indexes {
            if !members.is_empty() {
                self.store
                    .record_research_members(&index, &members, Utc::now())?;
            }
        }
        for note in out.notes {
            tracing::info!(note = %note, "research membership");
        }
        Ok(())
    }

    /// Research instruments (evidence-only, with membership) that have enough bars.
    /// Empty while the research universe is off. Claude, round 19.
    pub(super) fn research_universe(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<Instrument>, ServiceError> {
        let Some(indexes) = research_indexes() else {
            return Ok(Vec::new());
        };
        let decision = DecisionTime::at(now);
        let mut members: BTreeMap<String, crate::universe::ResearchMember> = BTreeMap::new();
        for index in indexes {
            if let Some(((_, stored), _)) =
                self.store
                    .research_members_as_of(&index, decision, VintagePolicy::ObservedOnly)?
            {
                for member in stored {
                    members.insert(member.yahoo.clone(), member);
                }
            }
        }
        let mut out = Vec::new();
        for (yahoo, member) in members {
            let key = research_key(&yahoo);
            let Some(bars) = self.bars_for_research(&key, now)? else {
                continue;
            };
            if !usable(&bars) {
                continue;
            }
            out.push(Instrument {
                key,
                symbol: yahoo.clone(),
                class: member.class,
                foreign_currency: false,
                bars,
                earnings: Vec::new(),
                evidence_only: true,
                membership: member.membership,
                context: self.trade_context(&yahoo, Some(&yahoo), member.class, now)?,
            });
        }
        Ok(out)
    }

    fn research_coverage(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<ResearchCoverage>, ServiceError> {
        let Some(indexes) = research_indexes() else {
            return Ok(None);
        };
        let mut members: BTreeMap<String, crate::universe::ResearchMember> = BTreeMap::new();
        let mut fetched_at = None;
        for index in &indexes {
            if let Some((at, stored)) = self.store.research_members(index)? {
                fetched_at = Some(fetched_at.map_or(at, |current: DateTime<Utc>| current.max(at)));
                for member in stored {
                    members.insert(member.yahoo.clone(), member);
                }
            }
        }
        let mut coverage = ResearchCoverage {
            indexes,
            fetched_at,
            ..ResearchCoverage::default()
        };
        for (yahoo, member) in members {
            let key = research_key(&yahoo);
            let bars = self.bars_for_research(&key, now)?.unwrap_or_default();
            let enough = bars.len() >= MIN_OHL_BARS;
            let former = !member.membership.is_empty()
                && member.membership.iter().all(|(_, to)| to.is_some());
            coverage.members += 1;
            if former {
                coverage.former += 1;
            }
            if enough {
                coverage.with_prices += 1;
                if former {
                    coverage.former_with_prices += 1;
                }
            }
            if enough && !usable(&bars) {
                coverage.excluded += 1;
            }
        }
        Ok(Some(coverage))
    }

    fn bars_for_research(
        &self,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Vec<Bar>>, ServiceError> {
        // Round 24: research names carry volume and, once fetched, dividends.
        let yahoo = key.strip_prefix("research:");
        self.trade_bars(key, yahoo, AssetClass::Stock, now, MIN_OHL_BARS)
    }
}

const RESEARCH_MAX_AGE_DAYS: i64 = 7;
const RESEARCH_MAX_FETCHES: usize = crate::market::MAX_INSTRUMENTS_PER_REFRESH;

fn research_key(yahoo: &str) -> String {
    format!("research:{}", yahoo.to_ascii_uppercase())
}

fn research_indexes() -> Option<Vec<String>> {
    let raw = std::env::var("SAIL_TRADE_RESEARCH").ok()?;
    let indexes: Vec<String> = raw
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "sp500" | "stoxx50"))
        .collect();
    (!indexes.is_empty()).then_some(indexes)
}

fn weekdays_since(from: NaiveDate, to: NaiveDate) -> u32 {
    if to <= from {
        return 0;
    }
    let mut date = from;
    let mut count = 0;
    while date < to {
        date = date + Days::new(1);
        if !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
            count += 1;
        }
    }
    count
}
