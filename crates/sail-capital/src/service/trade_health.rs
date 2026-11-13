//! Round 19: `GET /signals/trade/health` and the research universe (Claude's slice).
use super::{CapitalService, ServiceError};
use crate::contracts::AccountId;
use crate::trade::Instrument;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

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
    pub gaps: Vec<crate::trade::quality::Gap>,
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
        let _ = account;
        Ok(TradeHealth::default())
    }

    /// Research instruments (evidence-only, with membership) that have enough bars.
    /// Empty while the research universe is off. Claude, round 19.
    pub(super) fn research_universe(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<Instrument>, ServiceError> {
        let _ = now;
        Ok(Vec::new())
    }
}
