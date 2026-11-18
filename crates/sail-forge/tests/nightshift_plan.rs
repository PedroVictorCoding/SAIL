//! K3: deterministic night planning and duration history.

use chrono::{DateTime, Utc};
use sail_forge::nightshift::plan::{
    PlanErrand, PlanIssue, SizeDurations, durations_from_history, plan_night,
};
use sail_forge::nightshift::{ErrandId, NightItem, Size};

fn at(minutes: i64) -> DateTime<Utc> {
    let base: DateTime<Utc> = "2026-11-18T20:00:00Z".parse().expect("fixed time");
    base + chrono::Duration::minutes(minutes)
}

fn errand(seed: u8, size: Size, minutes: i64, redo: bool, attempts: u32) -> PlanErrand {
    PlanErrand {
        id: ErrandId::new([seed; 6]),
        size,
        approved_at: at(minutes),
        has_redo_note: redo,
        attempts,
    }
}

fn deferred_numbers(plan: &sail_forge::nightshift::NightPlan) -> Vec<u64> {
    plan.deferred
        .iter()
        .filter_map(|d| match d.item {
            NightItem::Issue { number } => Some(number),
            NightItem::Errand { .. } => None,
        })
        .collect()
}

#[test]
fn durations_use_the_last_ten_with_three_minimum() {
    let defaults = SizeDurations {
        s_secs: 1_800,
        m_secs: 3_600,
        issue_secs: 3_600,
    };
    assert_eq!(durations_from_history(&[]), defaults);
    assert_eq!(
        durations_from_history(&[(Size::S, 10), (Size::S, 20)]),
        defaults
    );

    let s = durations_from_history(&[(Size::S, 100), (Size::S, 300), (Size::S, 200)]);
    assert_eq!(s.s_secs, 200);
    assert_eq!(s.m_secs, 3_600);

    // Even count: integer median of the two middle values.
    let s = durations_from_history(&[
        (Size::S, 100),
        (Size::S, 200),
        (Size::S, 301),
        (Size::S, 400),
    ]);
    assert_eq!(s.s_secs, 250);

    // Only the last ten count.
    let mut history = Vec::new();
    for value in 1..=12u64 {
        history.push((Size::S, value));
    }
    let s = durations_from_history(&history);
    assert_eq!(s.s_secs, 7);

    // M is independent, L is ignored, issues keep the fixed default.
    let mixed = durations_from_history(&[
        (Size::S, 1),
        (Size::S, 2),
        (Size::S, 3),
        (Size::M, 5_000),
        (Size::M, 7_000),
        (Size::M, 9_000),
        (Size::L, 100),
    ]);
    assert_eq!(mixed.s_secs, 2);
    assert_eq!(mixed.m_secs, 7_000);
    assert_eq!(mixed.issue_secs, 3_600);

    // Three equal values are a valid measurement.
    let s = durations_from_history(&[(Size::S, 42), (Size::S, 42), (Size::S, 42)]);
    assert_eq!(s.s_secs, 42);
}

#[test]
fn redo_errands_then_approved_errands_then_issues() {
    let errands = vec![
        errand(1, Size::S, 30, false, 0),
        errand(2, Size::S, 10, false, 0),
        errand(3, Size::S, 60, true, 0),
    ];
    let issues = vec![PlanIssue { number: 300 }, PlanIssue { number: 125 }];
    let plan = plan_night(&errands, &issues, 6 * 3_600, 5, &SizeDurations::default());

    let expected = vec![
        NightItem::Errand {
            id: ErrandId::new([3; 6]),
        },
        NightItem::Errand {
            id: ErrandId::new([2; 6]),
        },
        NightItem::Errand {
            id: ErrandId::new([1; 6]),
        },
        NightItem::Issue { number: 125 },
        NightItem::Issue { number: 300 },
    ];
    assert_eq!(plan.items, expected);
    assert!(plan.deferred.is_empty());
}

#[test]
fn attempts_size_l_and_the_window_are_deferred_with_reasons() {
    let d = SizeDurations::default();
    // 90% of two hours is 6 480 s.
    let errands = vec![
        errand(1, Size::S, 1, false, 0), // 1 800
        errand(2, Size::M, 2, false, 0), // 6 480 after this? 1 800 + 3 600 = 5 400 fits
        errand(3, Size::S, 3, false, 0), // 5 400 + 1 800 = 7 200 does not fit
        errand(4, Size::L, 4, false, 0), // too big
        errand(5, Size::S, 5, false, 3), // three nights
        errand(6, Size::S, 6, false, 2), // 5 400 + 1 800 = 7 200 does not fit
    ];
    let plan = plan_night(&errands, &[], 7_200, 10, &d);
    assert_eq!(
        plan.items,
        vec![
            NightItem::Errand {
                id: ErrandId::new([1; 6])
            },
            NightItem::Errand {
                id: ErrandId::new([2; 6])
            }
        ]
    );
    let reasons: Vec<String> = plan.deferred.iter().map(|d| d.reason.clone()).collect();
    assert_eq!(
        reasons,
        vec![
            "doesn't fit in tonight's window",
            "too big for one night; split it",
            "tried on 3 nights; needs a new note",
            "doesn't fit in tonight's window",
        ]
    );
}

#[test]
fn a_later_small_item_still_fits_after_a_large_one_is_skipped() {
    let errands = vec![
        errand(1, Size::M, 1, false, 0), // 3 600
        errand(2, Size::S, 2, false, 0), // 1 800 → 5 400
        errand(3, Size::M, 3, false, 0), // 9 000 → does not fit
        errand(4, Size::S, 4, false, 0), // 7 200 → does not fit (budget 6 480)
    ];
    let plan = plan_night(&errands, &[], 7_200, 10, &SizeDurations::default());
    assert_eq!(plan.items.len(), 2);
    assert!(matches!(
        &plan.deferred[0].item,
        NightItem::Errand { id } if id.as_str() == ErrandId::new([3; 6]).as_str()
    ));

    // With a slightly wider window, the last small item does fit after the large one.
    let plan = plan_night(&errands, &[], 12_000, 10, &SizeDurations::default());
    assert_eq!(plan.items.len(), 4);
}

#[test]
fn a_redo_note_allows_a_fourth_attempt() {
    let errands = vec![errand(1, Size::S, 1, true, 3)];
    let plan = plan_night(&errands, &[], 5_000, 5, &SizeDurations::default());
    assert_eq!(plan.items.len(), 1);
    assert!(plan.deferred.is_empty());
}

#[test]
fn max_items_caps_errands_and_issues_together() {
    let errands = vec![
        errand(1, Size::S, 1, false, 0),
        errand(2, Size::S, 2, false, 0),
    ];
    let issues = vec![PlanIssue { number: 1 }, PlanIssue { number: 2 }];
    let plan = plan_night(&errands, &issues, 24 * 3_600, 3, &SizeDurations::default());
    assert_eq!(plan.items.len(), 3);
    assert_eq!(plan.deferred.len(), 1);
    assert_eq!(plan.deferred[0].reason, "over the per-night limit");
    assert_eq!(deferred_numbers(&plan), vec![2]);
}

#[test]
fn zero_items_or_zero_window_defer_everything() {
    let errands = vec![errand(1, Size::S, 1, false, 0)];
    let issues = vec![PlanIssue { number: 1 }];

    let none = plan_night(&errands, &issues, 3_600, 0, &SizeDurations::default());
    assert!(none.items.is_empty());
    assert_eq!(none.deferred.len(), 2);
    assert!(
        none.deferred
            .iter()
            .all(|d| d.reason == "over the per-night limit")
    );

    let no_window = plan_night(&errands, &issues, 0, 10, &SizeDurations::default());
    assert!(no_window.items.is_empty());
    assert!(
        no_window
            .deferred
            .iter()
            .all(|d| d.reason == "doesn't fit in tonight's window")
    );

    let no_inputs = plan_night(&[], &[], 3_600, 10, &SizeDurations::default());
    assert_eq!(no_inputs.items, Vec::new());
    assert_eq!(no_inputs.deferred, Vec::new());
}

#[test]
fn a_zero_duration_item_fits_any_non_zero_budget() {
    let d = SizeDurations {
        s_secs: 0,
        m_secs: 0,
        issue_secs: 3_600,
    };
    let plan = plan_night(&[errand(1, Size::S, 0, false, 0)], &[], 10, 10, &d);
    assert_eq!(plan.items.len(), 1);
}

#[test]
fn planning_is_deterministic_and_ties_break_by_id() {
    let errands = vec![
        errand(9, Size::S, 10, false, 0),
        errand(8, Size::S, 10, false, 0),
    ];
    let d = SizeDurations::default();
    let first = plan_night(&errands, &[], 5_000, 5, &d);
    let second = plan_night(&errands, &[], 5_000, 5, &d);
    assert_eq!(first, second);
    assert_eq!(
        first.items,
        vec![
            NightItem::Errand {
                id: ErrandId::new([8; 6])
            },
            NightItem::Errand {
                id: ErrandId::new([9; 6])
            }
        ]
    );
}
