//! K3: pure night planning.
//!
//! The plan is deterministic: redo errands first, then other approved errands by
//! `approved_at`, then admitted GitHub issues by number. An item is taken while the
//! expected total stays within 90% of the window (owner decision: leave slack for
//! verification and overruns) and the item count stays within `max_items`.

use chrono::{DateTime, Utc};

use super::{Deferred, ErrandId, NightItem, NightPlan, Size};

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

/// Expected wall time per size. The defaults S = 1 800 s and M = 3 600 s and issues
/// 3 600 s are fixed a priori; S and M are replaced by the median of the last ten
/// finished jobs of that size when at least three exist (a measurement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeDurations {
    pub s_secs: u64,
    pub m_secs: u64,
    pub issue_secs: u64,
}

impl Default for SizeDurations {
    fn default() -> Self {
        Self {
            s_secs: 1_800,
            m_secs: 3_600,
            issue_secs: 3_600,
        }
    }
}

/// Plan the night from approved errands and admitted issues. Pure.
pub fn plan_night(
    errands: &[PlanErrand],
    issues: &[PlanIssue],
    window_secs: u64,
    max_items: u8,
    d: &SizeDurations,
) -> NightPlan {
    let mut ordered: Vec<&PlanErrand> = errands.iter().collect();
    ordered.sort_by(|a, b| {
        b.has_redo_note
            .cmp(&a.has_redo_note)
            .then_with(|| a.approved_at.cmp(&b.approved_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut ordered_issues: Vec<&PlanIssue> = issues.iter().collect();
    ordered_issues.sort_by_key(|issue| issue.number);

    let budget = window_secs.saturating_mul(9) / 10;
    let mut items = Vec::new();
    let mut deferred = Vec::new();
    let mut used = 0u64;

    for errand in ordered {
        if errand.attempts >= 3 {
            deferred.push(Deferred {
                item: NightItem::Errand {
                    id: errand.id.clone(),
                },
                reason: "tried on 3 nights; needs a new note".into(),
            });
            continue;
        }
        if errand.size == Size::L {
            deferred.push(Deferred {
                item: NightItem::Errand {
                    id: errand.id.clone(),
                },
                reason: "too big for one night; split it".into(),
            });
            continue;
        }
        let duration = match errand.size {
            Size::S => d.s_secs,
            Size::M => d.m_secs,
            Size::L => 0,
        };
        if items.len() >= usize::from(max_items) {
            deferred.push(Deferred {
                item: NightItem::Errand {
                    id: errand.id.clone(),
                },
                reason: "over the per-night limit".into(),
            });
            continue;
        }
        if used.saturating_add(duration) > budget {
            deferred.push(Deferred {
                item: NightItem::Errand {
                    id: errand.id.clone(),
                },
                reason: "doesn't fit in tonight's window".into(),
            });
            continue;
        }
        used = used.saturating_add(duration);
        items.push(NightItem::Errand {
            id: errand.id.clone(),
        });
    }

    for issue in ordered_issues {
        if items.len() >= usize::from(max_items) {
            deferred.push(Deferred {
                item: NightItem::Issue {
                    number: issue.number,
                },
                reason: "over the per-night limit".into(),
            });
            continue;
        }
        if used.saturating_add(d.issue_secs) > budget {
            deferred.push(Deferred {
                item: NightItem::Issue {
                    number: issue.number,
                },
                reason: "doesn't fit in tonight's window".into(),
            });
            continue;
        }
        used = used.saturating_add(d.issue_secs);
        items.push(NightItem::Issue {
            number: issue.number,
        });
    }

    NightPlan { items, deferred }
}

/// Derive expected durations from the finished-job history. `finished` is in completion
/// order; only the last ten jobs of a size count, and a median is used only when at least
/// three exist. Issues always keep the fixed default.
pub fn durations_from_history(finished: &[(Size, u64)]) -> SizeDurations {
    let mut durations = SizeDurations::default();
    for (size, field) in [
        (Size::S, &mut durations.s_secs),
        (Size::M, &mut durations.m_secs),
    ] {
        let mut values: Vec<u64> = finished
            .iter()
            .filter(|(finished_size, _)| *finished_size == size)
            .map(|(_, secs)| *secs)
            .collect();
        if values.len() < 3 {
            continue;
        }
        if values.len() > 10 {
            values = values.split_off(values.len() - 10);
        }
        values.sort_unstable();
        let median = if values.len() % 2 == 1 {
            values[values.len() / 2]
        } else {
            let a = values[values.len() / 2 - 1];
            let b = values[values.len() / 2];
            a / 2 + b / 2 + (a % 2 + b % 2) / 2
        };
        *field = median;
    }
    durations
}
