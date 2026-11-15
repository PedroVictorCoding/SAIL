//! Round 24: portfolio heat across the day's cards (Kilo's slice, A6). **This module doc
//! is the spec.**
//!
//! Each card risks `risk_pct` of the account at its stop. Eight correlated longs are one
//! bet, so [`cap`] admits cards against two limits that count the cards still open too:
//! - **total:** the sum of `risk_pct` over open cards and admitted cards stays at or below
//!   [`HeatLimits::max_total_risk_pct`];
//! - **cluster:** instruments are clustered by single linkage, joining two keys when the
//!   Pearson correlation of their daily returns over the last 60 common sessions
//!   (at least 40) is above [`HeatLimits::cluster_corr`]; a key with too little history is
//!   its own cluster. Risk inside one cluster stays at or below
//!   [`HeatLimits::max_cluster_risk_pct`]. A long and a short in the same cluster offset:
//!   a cluster's risk is `|Σ long risk − Σ short risk|`.
//!
//! Order: verified cards first in their given (score) order, then candidates in theirs.
//! A card that would break a limit is left out with a reason starting
//! `"portfolio heat: "` and naming the limit, e.g.
//! `"portfolio heat: 3.0% already at risk (limit 3.0%)"`. Admitted cards keep their order.
//! The summary reports the risk after admission and the clusters with more than one key
//! or any risk.
//!
//! Pure: no I/O, no clock.

use std::collections::HashMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::{Side, SignalCard};

/// A published card that has not been graded yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenCard {
    /// Instrument key.
    pub key: String,
    /// Side.
    pub side: Side,
    /// Percent of the account at risk to its stop.
    pub risk_pct: f64,
}

/// Daily log returns per instrument key, oldest first, for correlations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CorrInputs {
    /// `(session, ln(close_t / close_{t−1}))` per key.
    pub returns: HashMap<String, Vec<(NaiveDate, f64)>>,
}

/// The heat limits, in percent of the account.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeatLimits {
    /// Most total risk across open and new cards.
    pub max_total_risk_pct: f64,
    /// Most net risk inside one correlated cluster.
    pub max_cluster_risk_pct: f64,
    /// Correlation above which two instruments join a cluster.
    pub cluster_corr: f64,
}

impl Default for HeatLimits {
    /// Drafts: 3% total, 1.5% per cluster, clusters above 0.7 correlation. Six trades at
    /// the draft 0.5% risk fill the total; a cluster holds three.
    fn default() -> Self {
        HeatLimits {
            max_total_risk_pct: 3.0,
            max_cluster_risk_pct: 1.5,
            cluster_corr: 0.7,
        }
    }
}

/// One correlated cluster.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cluster {
    /// Instrument keys, sorted.
    pub keys: Vec<String>,
    /// Net risk in the cluster after admission, percent.
    pub risk_pct: f64,
}

/// Heat after admission. JSON: `{total_risk_pct, limit_pct, cluster_limit_pct, clusters}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeatSummary {
    /// Open plus admitted risk, percent.
    pub total_risk_pct: f64,
    /// [`HeatLimits::max_total_risk_pct`].
    pub limit_pct: f64,
    /// [`HeatLimits::max_cluster_risk_pct`].
    pub cluster_limit_pct: f64,
    /// Clusters with more than one key or any risk, largest risk first.
    pub clusters: Vec<Cluster>,
}

/// What [`cap`] returns.
#[derive(Clone, Debug, PartialEq)]
pub struct HeatOutcome {
    /// Admitted verified cards, in input order.
    pub cards: Vec<SignalCard>,
    /// Admitted candidates, in input order.
    pub candidates: Vec<SignalCard>,
    /// Left-out cards with their `"portfolio heat: …"` reason.
    pub suppressed: Vec<(SignalCard, String)>,
    /// Heat after admission.
    pub summary: HeatSummary,
}

/// Kilo, round 24, *pure*: admits cards under the limits (module doc). M0 stub: admits
/// everything and reports only the total.
pub fn cap(
    cards: Vec<SignalCard>,
    candidates: Vec<SignalCard>,
    open: &[OpenCard],
    correlations: &CorrInputs,
    limits: HeatLimits,
) -> HeatOutcome {
    let _ = correlations;
    let total = open.iter().map(|c| c.risk_pct).sum::<f64>()
        + cards
            .iter()
            .chain(candidates.iter())
            .map(|c| c.risk_pct)
            .sum::<f64>();
    HeatOutcome {
        cards,
        candidates,
        suppressed: Vec::new(),
        summary: HeatSummary {
            total_risk_pct: total,
            limit_pct: limits.max_total_risk_pct,
            cluster_limit_pct: limits.max_cluster_risk_pct,
            clusters: Vec::new(),
        },
    }
}
