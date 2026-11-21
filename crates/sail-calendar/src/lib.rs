//! SAIL Calendar: local events, read-only ICS feeds, recurrence, import/export and the
//! deterministic agenda. No model call is made anywhere in this crate.

pub mod agenda;
pub mod contracts;
pub mod fetch;
pub mod ics;
pub mod recur;
pub mod service;
pub mod store;
pub mod sync;

pub use contracts::CalendarError;
