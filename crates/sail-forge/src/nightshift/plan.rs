//! K3: pure night planning.
//!
//! Stub in K0.

use chrono::{DateTime, Utc};

use super::{ErrandId, NightItem, NightPlan, Size};

/// One approved errand offered to the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanErrand {
    pub id: ErrandId,
    pub size: Size,
    pub approved_at: DateTime<Utc>,
    pub has_redo_note: bool,
    pub attempts: u32,
}

/// An already-admitted `forge-ready` issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanIssue {
    pub number: u64,
}

/// Expected wall time per size. Default S = 1800 s and M = 3600 s (fixed a priori);
/// replaced by the median of the last ten finished jobs of that size when at least three
/// exist (a measurement). Issues default to 3600 s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeDurations {
    pub s_secs: u64,
    pub m_secs: u64,
    pub issue_secs: u64,
}

/// Plan the night. Stub until K3.
pub fn plan_night(
    errands: &[PlanErrand],
    issues: &[PlanIssue],
    window_secs: u64,
    max_items: u8,
    d: &SizeDurations,
) -> NightPlan {
    let _ = (errands, issues, window_secs, max_items, d);
    NightPlan {
        items: Vec::<NightItem>::new(),
        deferred: Vec::new(),
    }
}

/// Derive expected durations from finished job history. Stub until K3.
pub fn durations_from_history(finished: &[(Size, u64)]) -> SizeDurations {
    let _ = finished;
    SizeDurations {
        s_secs: 1800,
        m_secs: 3600,
        issue_secs: 3600,
    }
}
