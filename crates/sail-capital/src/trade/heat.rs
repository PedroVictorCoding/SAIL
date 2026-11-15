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

use std::collections::{BTreeSet, HashMap};

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

/// Risk counted for a card or open position: a non-finite or negative value counts 0.
fn risk(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

/// Signed risk: longs add, shorts subtract (module doc).
fn signed(side: Side, value: f64) -> f64 {
    match side {
        Side::Long => risk(value),
        Side::Short => -risk(value),
    }
}

/// Kilo, round 24, *pure*: admits cards under the limits (module doc).
pub fn cap(
    cards: Vec<SignalCard>,
    candidates: Vec<SignalCard>,
    open: &[OpenCard],
    correlations: &CorrInputs,
    limits: HeatLimits,
) -> HeatOutcome {
    let mut keys: Vec<String> = open
        .iter()
        .map(|card| card.key.clone())
        .chain(cards.iter().map(|card| card.key.clone()))
        .chain(candidates.iter().map(|card| card.key.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    let clusters = single_linkage(&keys, correlations, limits.cluster_corr);
    let mut cluster_of: HashMap<&str, usize> = HashMap::new();
    for (index, members) in clusters.iter().enumerate() {
        for key in members {
            cluster_of.insert(key.as_str(), index);
        }
    }
    let cluster_of = |key: &str| cluster_of.get(key).copied().unwrap_or(0);

    let mut total = open.iter().map(|card| risk(card.risk_pct)).sum::<f64>();
    let mut net = vec![0.0_f64; clusters.len()];
    for card in open {
        net[cluster_of(&card.key)] += signed(card.side, card.risk_pct);
    }

    let mut admitted_cards = Vec::new();
    let mut admitted_candidates = Vec::new();
    let mut suppressed = Vec::new();
    let mut admit = |cards: Vec<SignalCard>, admitted: &mut Vec<SignalCard>| {
        for card in cards {
            let value = risk(card.risk_pct);
            if total + value > limits.max_total_risk_pct + 1e-9 {
                suppressed.push((
                    card,
                    format!(
                        "portfolio heat: {:.1}% already at risk (limit {:.1}%)",
                        total, limits.max_total_risk_pct
                    ),
                ));
                continue;
            }
            let at = cluster_of(&card.key);
            let cluster = net[at] + signed(card.side, card.risk_pct);
            if cluster.abs() > limits.max_cluster_risk_pct + 1e-9 {
                suppressed.push((
                    card,
                    format!(
                        "portfolio heat: {:.1}% in a correlated cluster (limit {:.1}%)",
                        cluster.abs(),
                        limits.max_cluster_risk_pct
                    ),
                ));
                continue;
            }
            total += value;
            net[at] = cluster;
            admitted.push(card);
        }
    };
    admit(cards, &mut admitted_cards);
    admit(candidates, &mut admitted_candidates);

    let mut active: BTreeSet<&str> = BTreeSet::new();
    for card in open {
        active.insert(card.key.as_str());
    }
    for card in admitted_cards.iter().chain(admitted_candidates.iter()) {
        active.insert(card.key.as_str());
    }
    let mut summary_clusters: Vec<Cluster> = clusters
        .iter()
        .enumerate()
        .filter_map(|(index, members)| {
            let members: Vec<String> = members
                .iter()
                .filter(|key| active.contains(key.as_str()))
                .cloned()
                .collect();
            if members.len() <= 1 && net[index].abs() <= 0.0 {
                return None;
            }
            let mut keys = members;
            keys.sort();
            Some(Cluster {
                keys,
                risk_pct: net[index].abs(),
            })
        })
        .collect();
    summary_clusters.sort_by(|left, right| {
        right
            .risk_pct
            .total_cmp(&left.risk_pct)
            .then(left.keys.cmp(&right.keys))
    });

    HeatOutcome {
        cards: admitted_cards,
        candidates: admitted_candidates,
        suppressed,
        summary: HeatSummary {
            total_risk_pct: total,
            limit_pct: limits.max_total_risk_pct,
            cluster_limit_pct: limits.max_cluster_risk_pct,
            clusters: summary_clusters,
        },
    }
}

/// Single-linkage clusters over `keys`, sorted member lists in input key order.
fn single_linkage(keys: &[String], correlations: &CorrInputs, threshold: f64) -> Vec<Vec<String>> {
    let mut parent: Vec<usize> = (0..keys.len()).collect();
    for left in 0..keys.len() {
        for right in left + 1..keys.len() {
            let (Some(first), Some(second)) = (
                correlations.returns.get(&keys[left]),
                correlations.returns.get(&keys[right]),
            ) else {
                continue;
            };
            if correlation(first, second).is_some_and(|value| value > threshold) {
                union(&mut parent, left, right);
            }
        }
    }
    let mut groups: HashMap<usize, Vec<String>> = HashMap::new();
    for (index, key) in keys.iter().enumerate() {
        groups
            .entry(find(&mut parent, index))
            .or_default()
            .push(key.clone());
    }
    let mut clusters: Vec<Vec<String>> = groups.into_values().collect();
    clusters.sort();
    clusters
}

fn find(parent: &mut [usize], mut index: usize) -> usize {
    while parent[index] != index {
        parent[index] = parent[parent[index]];
        index = parent[index];
    }
    index
}

fn union(parent: &mut [usize], left: usize, right: usize) {
    let left = find(parent, left);
    let right = find(parent, right);
    if left != right {
        parent[right] = left;
    }
}

/// Pearson correlation over the last 60 common sessions, `None` with fewer than 40.
fn correlation(first: &[(NaiveDate, f64)], second: &[(NaiveDate, f64)]) -> Option<f64> {
    let mut by_date: HashMap<NaiveDate, f64> = HashMap::with_capacity(second.len());
    for (session, value) in second {
        if value.is_finite() {
            by_date.insert(*session, *value);
        }
    }
    let mut pairs: Vec<(NaiveDate, f64, f64)> = first
        .iter()
        .filter(|(session, value)| value.is_finite() && by_date.contains_key(session))
        .map(|(session, value)| (*session, *value, by_date[session]))
        .collect();
    pairs.sort_by_key(|(session, _, _)| *session);
    if pairs.len() > 60 {
        pairs.drain(0..pairs.len() - 60);
    }
    if pairs.len() < 40 {
        return None;
    }
    let count = pairs.len() as f64;
    let mean_first = pairs.iter().map(|(_, first, _)| first).sum::<f64>() / count;
    let mean_second = pairs.iter().map(|(_, _, second)| second).sum::<f64>() / count;
    let mut covariance = 0.0;
    let mut variance_first = 0.0;
    let mut variance_second = 0.0;
    for (_, first, second) in &pairs {
        let first = first - mean_first;
        let second = second - mean_second;
        covariance += first * second;
        variance_first += first * first;
        variance_second += second * second;
    }
    if variance_first <= 0.0 || variance_second <= 0.0 {
        return None;
    }
    let value = covariance / (variance_first * variance_second).sqrt();
    value.is_finite().then(|| value.clamp(-1.0, 1.0))
}
