//! Exchange calendar abstraction for session labels and origin selection.
//!
//! [`WeekdayCalendar`] models Monday–Friday sessions without pretending to hold a
//! complete global holiday schedule. Callers or fixtures that need exchange-specific
//! holidays supply an explicit [`HolidayCalendar`].

use crate::contracts::DecisionTime;
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use std::collections::BTreeSet;

/// Bound on calendar walk-back / walk-forward loops so malformed calendars never hang.
const MAX_CALENDAR_STEP_DAYS: i64 = 60;

/// Abstraction over an exchange's trading-session schedule.
pub trait ExchangeCalendar: Send + Sync {
    /// Stable identifier for manifests and diagnostics (e.g. `"weekday"` or `"xnys-synth"`).
    fn id(&self) -> &str;

    /// True when `date` is a trading session on this calendar.
    fn is_session(&self, date: NaiveDate) -> bool;

    /// Latest valid session on or before `date`.
    fn session_on_or_before(&self, mut date: NaiveDate) -> Option<NaiveDate> {
        for _ in 0..=MAX_CALENDAR_STEP_DAYS {
            if self.is_session(date) {
                return Some(date);
            }
            date = date.pred_opt()?;
        }
        None
    }

    /// Earliest valid session strictly after `date`.
    fn next_session(&self, mut date: NaiveDate) -> Option<NaiveDate> {
        for _ in 0..MAX_CALENDAR_STEP_DAYS {
            date = date.succ_opt()?;
            if self.is_session(date) {
                return Some(date);
            }
        }
        None
    }

    /// Steps `count` trading sessions forward (`count > 0`) or backward (`count < 0`).
    /// When `count == 0`, returns [`Self::session_on_or_before`].
    fn add_sessions(&self, mut date: NaiveDate, count: i64) -> Option<NaiveDate> {
        if count == 0 {
            return self.session_on_or_before(date);
        }
        let step = if count > 0 { 1 } else { -1 };
        let mut remaining = count.abs();
        let mut guard = 0i64;
        while remaining > 0 {
            guard += 1;
            if guard > remaining.saturating_mul(10).max(MAX_CALENDAR_STEP_DAYS) {
                return None;
            }
            date += Duration::days(step);
            if self.is_session(date) {
                remaining -= 1;
            }
        }
        Some(date)
    }

    /// Number of valid sessions in `(from, to]`. Returns `0` when `to <= from`.
    fn sessions_between(&self, from: NaiveDate, to: NaiveDate) -> u32 {
        if to <= from {
            return 0;
        }
        let mut count = 0u32;
        let mut cur = from;
        while cur < to {
            let Some(next) = cur.succ_opt() else {
                break;
            };
            cur = next;
            if self.is_session(cur) {
                count += 1;
            }
        }
        count
    }

    /// Selects the origin `(index, session)` from an ascending slice of bar sessions at
    /// `decision`. Snaps `decision.session` to [`Self::session_on_or_before`], then picks
    /// the latest bar session `<= snapped` that is itself a valid session on this calendar.
    fn select_origin(
        &self,
        sessions: &[NaiveDate],
        decision: DecisionTime,
    ) -> Option<(usize, NaiveDate)> {
        let snapped = self.session_on_or_before(decision.session)?;
        sessions
            .iter()
            .enumerate()
            .rev()
            .find(|(_, s)| **s <= snapped && self.is_session(**s))
            .map(|(idx, s)| (idx, *s))
    }
}

/// Default Monday–Friday calendar (no hard-coded holiday list).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WeekdayCalendar;

impl ExchangeCalendar for WeekdayCalendar {
    fn id(&self) -> &str {
        "weekday"
    }

    fn is_session(&self, date: NaiveDate) -> bool {
        !matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
    }
}

/// Exchange calendar with an explicit caller-supplied holiday set (used by synthetic
/// fixtures and configured exchange schedules; never hard-coded as complete).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HolidayCalendar {
    id: String,
    holidays: BTreeSet<NaiveDate>,
}

impl HolidayCalendar {
    /// Creates a named calendar excluding weekends and `holidays`.
    pub fn new(id: impl Into<String>, holidays: impl IntoIterator<Item = NaiveDate>) -> Self {
        Self {
            id: id.into(),
            holidays: holidays.into_iter().collect(),
        }
    }

    /// The configured holiday dates.
    pub fn holidays(&self) -> &BTreeSet<NaiveDate> {
        &self.holidays
    }
}

impl ExchangeCalendar for HolidayCalendar {
    fn id(&self) -> &str {
        &self.id
    }

    fn is_session(&self, date: NaiveDate) -> bool {
        !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) && !self.holidays.contains(&date)
    }
}
