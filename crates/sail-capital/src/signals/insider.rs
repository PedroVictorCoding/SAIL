//! Round 25 (Antigravity's slice): the insider-buying signal, from parsed Form 4s.
//!
//! **Value.** For instrument `j` at session `t`, over the Form 4s *published* (SEC's
//! acceptance time, [`crate::sec::form4::Form4::published_at`]) on or before the close
//! of `t` and whose transactions fall in the last [`WINDOW_SESSIONS`] sessions up to
//! `t`:
//! - `buyers` = distinct owners (officers or directors only; 10% owners who are
//!   neither are ignored) with an open-market purchase;
//! - `sellers` = the same for open-market sales;
//! - the raw value is `buyers − 0.5·sellers` (sales are weaker evidence: they happen for
//!   taxes, diversification, plans), and `+ CLUSTER_BONUS` when `buyers ≥
//!   CLUSTER_MIN_INSIDERS`.
//!
//! A value is emitted for every session of `j` in `sessions` from the first Form 4
//! known for it on (0 where nothing is in the window), so the cross-section sees zeros,
//! not missing values, for quiet weeks. Amendments (`4/A`) replace the original with
//! the same owner and transaction date and are never counted twice. Instruments with no
//! Form 4 known at all get no values (unknown, not zero).
//!
//! The values enter [`super::evaluate_with`] as [`super::ExternalScores`] for
//! [`super::SignalId::InsiderBuying`] and are scored like any other signal: no weight
//! unless the walk-forward IC is reliably above zero.
//!
//! **Purity.** No I/O and no clock.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::sec::form4::Form4;

/// Sessions a transaction counts for.
pub const WINDOW_SESSIONS: usize = 10;

/// Distinct buying insiders that make a cluster.
pub const CLUSTER_MIN_INSIDERS: usize = 3;

/// Added to the raw value for a cluster.
pub const CLUSTER_BONUS: f64 = 2.0;

/// The signal name the values are stored under (as [`super::SignalId::InsiderBuying`]
/// serializes) and the producer's version.
pub const STORED_SIGNAL: &str = "insider_buying";

/// Producer version stored with the values; bump when the formula changes.
pub const PRODUCER_VERSION: &str = "form4-cluster/1";

/// `(instrument key, session, value)` for every instrument with Form 4s.
/// `filings` maps instrument key → its parsed Form 4s; `sessions` maps instrument key →
/// its trading sessions, oldest first. M0 stub: no values.
pub fn cluster_scores(
    filings: &BTreeMap<String, Vec<Form4>>,
    sessions: &BTreeMap<String, Vec<NaiveDate>>,
) -> Vec<(String, NaiveDate, f64)> {
    let _ = (filings, sessions);
    Vec::new()
}

/// Today's clusters for the UI: `(instrument key, distinct buyers)` where the latest
/// session's buyers reach [`CLUSTER_MIN_INSIDERS`]. M0 stub: none.
pub fn current_clusters(
    filings: &BTreeMap<String, Vec<Form4>>,
    sessions: &BTreeMap<String, Vec<NaiveDate>>,
) -> Vec<(String, usize)> {
    let _ = (filings, sessions);
    Vec::new()
}
