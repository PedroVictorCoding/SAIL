//! SAIL Capital: the owner's private, read-only investment ledger (issue #422).
//!
//! Round 7 builds the first vertical slice: import official XTB report exports, keep an
//! append-only ledger with row-level provenance, project positions, cash, dividends and
//! fees from it, and reconcile against what the broker itself reported. There is no
//! market-data feed, no model and no broker connection: SAIL never places, modifies or
//! cancels orders and never holds broker credentials. See `docs/SAIL_CAPITAL.md`.
pub mod board;
pub mod board_record;
pub mod calendar;
pub mod contracts;
pub mod dividends;
pub mod evidence;
pub mod exposure;
pub mod forecast;
pub mod import;
pub mod insights;
pub mod ledger;
pub mod market;
pub mod markets;
pub mod models;
pub mod optimize;
pub mod outlook;
pub mod policy;
pub mod prediction;
pub mod replay;
pub mod scorecard;
pub mod sec;
pub mod service;
pub mod signals;
pub mod store;
pub mod suggest;
pub mod trade;
pub mod universe;

pub use contracts::*;
