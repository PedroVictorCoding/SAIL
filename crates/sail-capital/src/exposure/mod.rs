//! From filings to holdings (Antigravity's slice, round 8).
//!
//! These are pure, deterministic functions with no model and no I/O. An instrument maps to
//! an issuer only through an exact ticker match in a recorded map version. A filing becomes
//! a [`WatchItem`] only if it is visible at the [`AsOf`] and the owner held the issuer then.
//! The outcome is watch or research proposal, never an order.
use crate::evidence::*;
use crate::ledger::Projection;
use crate::{AsOf, Instrument};

/// Links one instrument to a SEC issuer. M0 stub; Antigravity replaces the body.
pub fn match_issuer(instrument: &Instrument, index: &TickerIndex) -> IssuerMatch {
    let _ = (instrument, index);
    IssuerMatch::Unresolved {
        reason: "not implemented yet (M0 stub)".into(),
    }
}

/// Decides whether a filing matters, and how much. `None` means "don't surface it". M0
/// stub; Antigravity replaces the body.
pub fn classify(filing: &Filing) -> Option<(ResearchAction, String)> {
    let _ = filing;
    None
}

/// Turns the visible evidence into watch items for the positions in `projection`. M0
/// stub; Antigravity replaces the body.
pub fn watch_items(
    projection: &Projection,
    matches: &[(String, IssuerMatch)],
    evidence: &[EvidenceRecord],
    as_of: AsOf,
) -> Vec<WatchItem> {
    let _ = (projection, matches, evidence, as_of);
    Vec::new()
}
