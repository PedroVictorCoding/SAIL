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

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;

use crate::sec::form4::Form4;

/// Sessions a transaction counts for (fixed a priori).
pub const WINDOW_SESSIONS: usize = 10;

/// Distinct buying insiders that make a cluster (fixed a priori).
pub const CLUSTER_MIN_INSIDERS: usize = 3;

/// Added to the raw value for a cluster (fixed a priori).
pub const CLUSTER_BONUS: f64 = 2.0;

/// Weight applied to distinct open-market sellers relative to buyers (fixed a priori).
const SELLER_WEIGHT: f64 = 0.5;

/// The signal name the values are stored under (as [`super::SignalId::InsiderBuying`]
/// serializes) and the producer's version.
pub const STORED_SIGNAL: &str = "insider_buying";

/// Producer version stored with the values; bump when the formula changes.
pub const PRODUCER_VERSION: &str = "form4-cluster/1";

/// `(instrument key, session, value)` for every instrument with Form 4s.
/// `filings` maps instrument key → its parsed Form 4s; `sessions` maps instrument key →
/// its trading sessions, oldest first.
pub fn cluster_scores(
    filings: &BTreeMap<String, Vec<Form4>>,
    sessions: &BTreeMap<String, Vec<NaiveDate>>,
) -> Vec<(String, NaiveDate, f64)> {
    let mut out = Vec::new();
    for (key, key_sessions) in sessions {
        let Some(key_filings) = filings.get(key) else {
            continue;
        };
        if key_filings.is_empty() || key_sessions.is_empty() {
            continue;
        }
        let mut sorted_filings = key_filings.clone();
        sorted_filings.sort_by(|a, b| {
            a.published_at
                .cmp(&b.published_at)
                .then(a.amendment.cmp(&b.amendment))
                .then(a.accession.cmp(&b.accession))
        });

        let first_known = sorted_filings[0].published_at;
        for (idx, &session) in key_sessions.iter().enumerate() {
            let close = session_close_utc(session);
            if close < first_known {
                continue;
            }
            let window_start = key_sessions[idx.saturating_sub(WINDOW_SESSIONS - 1)];
            let (buyers, sellers) = count_window(&sorted_filings, close, window_start, session);
            let bonus = if buyers >= CLUSTER_MIN_INSIDERS {
                CLUSTER_BONUS
            } else {
                0.0
            };
            let value = buyers as f64 - SELLER_WEIGHT * sellers as f64 + bonus;
            out.push((key.clone(), session, value));
        }
    }
    out
}

/// Today's clusters for the UI: `(instrument key, distinct buyers)` where the latest
/// session's buyers reach [`CLUSTER_MIN_INSIDERS`].
pub fn current_clusters(
    filings: &BTreeMap<String, Vec<Form4>>,
    sessions: &BTreeMap<String, Vec<NaiveDate>>,
) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (key, key_sessions) in sessions {
        let Some(&latest) = key_sessions.last() else {
            continue;
        };
        let Some(key_filings) = filings.get(key) else {
            continue;
        };
        if key_filings.is_empty() {
            continue;
        }
        let mut sorted_filings = key_filings.clone();
        sorted_filings.sort_by(|a, b| {
            a.published_at
                .cmp(&b.published_at)
                .then(a.amendment.cmp(&b.amendment))
                .then(a.accession.cmp(&b.accession))
        });
        let close = session_close_utc(latest);
        let window_start = key_sessions[key_sessions.len().saturating_sub(WINDOW_SESSIONS)];
        let (buyers, _) = count_window(&sorted_filings, close, window_start, latest);
        if buyers >= CLUSTER_MIN_INSIDERS {
            out.push((key.clone(), buyers));
        }
    }
    out
}

/// 16:00 America/New_York on `session` in UTC.
fn session_close_utc(session: NaiveDate) -> DateTime<Utc> {
    let naive = session
        .and_hms_opt(16, 0, 0)
        .expect("16:00:00 is always a valid time");
    New_York
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| New_York.from_local_datetime(&naive).latest())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&session.and_hms_opt(21, 0, 0).expect("valid")))
}

/// Counts distinct officer/director buyers and sellers in `[window_start, session]` from
/// filings published on or before `close`. Amendments (`4/A`) replace earlier entries for
/// the same `(owner, transaction_date)`.
fn count_window(
    sorted_filings: &[Form4],
    close: DateTime<Utc>,
    window_start: NaiveDate,
    session: NaiveDate,
) -> (usize, usize) {
    // Key: (owner_id, tx_date) -> (has_buy, has_sell)
    let mut by_owner_date: BTreeMap<(String, NaiveDate), (bool, bool)> = BTreeMap::new();

    for f in sorted_filings {
        if f.published_at > close {
            break;
        }
        let owner_ids: Vec<String> = f
            .owners
            .iter()
            .filter(|o| o.is_director || o.is_officer)
            .map(|o| {
                if !o.cik.is_empty() {
                    o.cik.clone()
                } else {
                    o.name.clone()
                }
            })
            .filter(|id| !id.is_empty())
            .collect();
        if owner_ids.is_empty() {
            continue;
        }

        // Aggregate within this filing per transaction date so multiple rows on the
        // same date in one filing combine, while a later 4/A replaces the prior filing.
        let mut filing_dates: BTreeMap<NaiveDate, (bool, bool)> = BTreeMap::new();
        for tx in &f.transactions {
            let buy = tx.is_open_market_buy();
            let sell = tx.is_open_market_sell();
            if !buy && !sell {
                continue;
            }
            let entry = filing_dates.entry(tx.date).or_insert((false, false));
            entry.0 |= buy;
            entry.1 |= sell;
        }

        for owner_id in &owner_ids {
            for (&tx_date, &(buy, sell)) in &filing_dates {
                if f.amendment {
                    by_owner_date.insert((owner_id.clone(), tx_date), (buy, sell));
                } else {
                    let entry = by_owner_date
                        .entry((owner_id.clone(), tx_date))
                        .or_insert((false, false));
                    entry.0 |= buy;
                    entry.1 |= sell;
                }
            }
        }
    }

    let mut buyers = BTreeSet::new();
    let mut sellers = BTreeSet::new();
    for ((owner_id, tx_date), (buy, sell)) in by_owner_date {
        if tx_date < window_start || tx_date > session {
            continue;
        }
        if buy {
            buyers.insert(owner_id.clone());
        }
        if sell {
            sellers.insert(owner_id);
        }
    }
    (buyers.len(), sellers.len())
}
