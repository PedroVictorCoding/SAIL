//! K4: the deterministic morning report.
//!
//! Stub in K0.

use std::collections::BTreeMap;

use super::{Errand, MorningReport, NightPlan};

/// Compose the morning report. Stub until K4.
pub fn compose_morning(
    date: &str,
    plan: &NightPlan,
    errands: &[Errand],
    issue_titles: &BTreeMap<u64, String>,
    halted: Option<String>,
) -> MorningReport {
    let _ = (plan, errands, issue_titles);
    MorningReport {
        date: date.to_string(),
        ready: 0,
        needs_you: 0,
        failed: 0,
        not_reached: 0,
        items: Vec::new(),
        spoken: String::new(),
        halted,
    }
}
