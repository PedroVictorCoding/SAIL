//! Round 25 (B8, Kilo's slice): the dividend projection's own track record.
//!
//! **Snapshots.** Once a month (the first run in a calendar month) the service stores
//! what the projection expected for the next 12 months: per holding, the base
//! scenario's year-1 net income ([`super::HoldingRow::first_year_net`]) with the units
//! held; for the portfolio, the low/base/high year-1 net and, when the Monte Carlo ran,
//! its p10/p90. Snapshots are append-only and never recomputed.
//!
//! **Grading** ([`grade`]), only once 12 months have passed since the snapshot's
//! `month`:
//! - received = the net receipts in `[month, month + 12 months)` of the holdings in the
//!   snapshot, each scaled by `snapshot units / units held when paid` (so buying or
//!   selling later doesn't count as a forecast error; a receipt with 0 units then is
//!   skipped);
//! - per holding and for the portfolio: `error = (received − projected) / projected`
//!   (skipped when projected ≤ 0);
//! - the portfolio's band: whether received fell inside `[low, high]` and, when present,
//!   inside `[p10, p90]`.
//!
//! **Summary.** `n` graded snapshots, mean signed error (bias), mean absolute error,
//! and the share inside each band. The scorecard's Income family reads it (Kilo's
//! `scorecard::dividend_record_row`: Earned with ≥ 12 graded snapshots, |bias| ≤ 0.10
//! and p10–p90 coverage in 0.65–0.95; Provisional below 12; otherwise Failing).
//!
//! **Purity.** No I/O and no clock.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// One holding's expectation in a snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotHolding {
    /// Ledger key.
    pub key: String,
    /// Symbol.
    pub symbol: String,
    /// Units held when the snapshot was made.
    pub units: f64,
    /// Expected net income over the next 12 months, account currency.
    pub projected_net: f64,
}

/// What the projection expected, as of one month.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IncomeSnapshot {
    /// First day of the month it covers from.
    pub month: NaiveDate,
    /// When it was made.
    pub made_at: DateTime<Utc>,
    /// Account currency.
    pub currency: String,
    /// Per holding (base scenario).
    pub holdings: Vec<SnapshotHolding>,
    /// Portfolio year-1 net in the low, base and high scenarios.
    pub low: f64,
    /// Base.
    pub base: f64,
    /// High.
    pub high: f64,
    /// Monte Carlo 10th percentile of year-1 net, when it ran.
    #[serde(default)]
    pub p10: Option<f64>,
    /// Monte Carlo 90th percentile.
    #[serde(default)]
    pub p90: Option<f64>,
}

/// One received dividend, net, in the account currency.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    /// Ledger key.
    pub key: String,
    /// Payment date.
    pub paid: NaiveDate,
    /// Net amount.
    pub net: f64,
    /// Units held when paid.
    pub units_then: f64,
}

/// A snapshot after its 12 months.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradedSnapshot {
    /// The snapshot's month.
    pub month: NaiveDate,
    /// Projected (base).
    pub projected: f64,
    /// Received, unit-adjusted.
    pub received: f64,
    /// `(received − projected) / projected`, when projected > 0.
    pub error: Option<f64>,
    /// Inside `[low, high]`.
    pub in_scenarios: bool,
    /// Inside `[p10, p90]`, when the snapshot had them.
    pub in_band: Option<bool>,
    /// Per holding: `(symbol, projected, received, error)`.
    pub holdings: Vec<(String, f64, f64, Option<f64>)>,
}

/// The projection's record.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordSummary {
    /// Graded snapshots, oldest first.
    pub graded: Vec<GradedSnapshot>,
    /// Snapshots still inside their 12 months.
    pub pending: u32,
    /// Mean signed error.
    pub bias: Option<f64>,
    /// Mean absolute error.
    pub mean_abs_error: Option<f64>,
    /// Share of graded snapshots inside `[low, high]`.
    pub scenario_coverage: Option<f64>,
    /// Share inside `[p10, p90]`, over those that had a band.
    pub band_coverage: Option<f64>,
    /// Plain notes.
    pub notes: Vec<String>,
}

/// The snapshot for `month` from a base-scenario projection: per-holding year-1 net
/// (`holdings` as `(key, symbol, units, first_year_net)`), the scenarios' year-1 totals
/// and the Monte Carlo band. M0 stub: builds it directly; Kilo may keep it.
#[allow(clippy::too_many_arguments)]
pub fn snapshot(
    month: NaiveDate,
    made_at: DateTime<Utc>,
    currency: &str,
    holdings: Vec<SnapshotHolding>,
    low: f64,
    base: f64,
    high: f64,
    band: Option<(f64, f64)>,
) -> IncomeSnapshot {
    IncomeSnapshot {
        month,
        made_at,
        currency: currency.to_string(),
        holdings,
        low,
        base,
        high,
        p10: band.map(|b| b.0),
        p90: band.map(|b| b.1),
    }
}

/// Grades every snapshot whose 12 months ended by `today`. M0 stub: everything
/// pending.
pub fn grade(
    snapshots: &[IncomeSnapshot],
    receipts: &[Receipt],
    today: NaiveDate,
) -> RecordSummary {
    let _ = (receipts, today);
    RecordSummary {
        pending: snapshots.len() as u32,
        notes: vec!["The projection record is not graded yet.".into()],
        ..RecordSummary::default()
    }
}
