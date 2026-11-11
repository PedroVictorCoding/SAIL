//! Suggestions for the "Today" view (round 11, CAP-18/22).
//!
//! Three kinds, always kept apart:
//! - **Attention**: something to look at, from fixed rules (stale data, unpriced
//!   holdings, reconciliation gaps, new filings, an unconfirmed policy). There is no
//!   sizing.
//! - **Policy**: a limit the owner set is broken, with the smallest change that fixes
//!   it (`optimize::Mode::Policy`). These are sized only against a **confirmed** policy.
//! - **Idea**: a change that validated signals favour after risk and cost
//!   (`optimize::Mode::Ideas`). Ideas exist only when a signal passed validation and the
//!   net benefit is above zero. Each one shows its evidence and confidence, and how it
//!   compares with doing nothing.
//!
//! Nothing here places an order. The owner can dismiss a suggestion or mark it done.
//! Its id hashes its kind, subject and evidence, so a dismissed suggestion comes back
//! only when the evidence changes.
pub mod build;

use crate::optimize::{Mode, Plan, Rule};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Look at this.
    Attention,
    /// A limit is broken.
    Policy,
    /// A validated signal favours a change.
    Idea,
}

/// How much it matters. The list is sorted by severity, then kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth acting on soon.
    High,
    /// Worth a look.
    Medium,
    /// For information.
    Low,
}

/// Where the page should take the owner for detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    /// Today.
    Today,
    /// Positions, CFDs, reconciliation.
    Holdings,
    /// Dividends and income.
    Income,
    /// Forecasts, the scorecard and signal skill.
    Forecasts,
    /// SEC filings.
    Research,
    /// Imports, prices and the policy editor.
    Data,
}

/// One sized change inside a policy or idea suggestion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SizedChange {
    /// Broker symbol.
    pub symbol: String,
    /// Share before, percent.
    pub from_pct: f64,
    /// Share after, percent.
    pub to_pct: f64,
    /// Signed amount in the account currency (positive adds).
    pub amount: f64,
}

/// A suggestion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    /// Stable id: a hash of kind, subject and evidence.
    pub id: String,
    /// Kind.
    pub kind: Kind,
    /// Severity.
    pub severity: Severity,
    /// One short line, e.g. "Free funds are 37% of the account; your limit is 15%".
    pub title: String,
    /// Why, in two or three plain sentences.
    pub why: String,
    /// What happens if the owner does nothing, for policy and idea suggestions.
    pub if_nothing: Option<String>,
    /// Sized changes (policy and idea suggestions).
    pub changes: Vec<SizedChange>,
    /// The rules involved (policy and idea suggestions).
    pub rules: Vec<Rule>,
    /// Ideas: "low", "medium" or "high", from the combined IC and the t-statistics.
    pub confidence: Option<String>,
    /// Short references to what it rests on, e.g. `policy v3`, `signal momentum_12_1 h60
    /// t=2.4`, `filing 0000320193-26-000101`, `import 2026-11-10`.
    pub evidence: Vec<String>,
    /// Where the detail is.
    pub tab: Tab,
}

/// What the owner did with a suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Hide it until its evidence changes.
    Dismissed,
    /// The owner acted on it (outside SAIL).
    Done,
}

/// A stored action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionRecord {
    /// Suggestion id.
    pub suggestion_id: String,
    /// The action.
    pub action: Action,
    /// When.
    pub at: DateTime<Utc>,
}

/// The overall stance shown above the list. It is derived, never asserted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stance {
    /// "hold", "fix_limits" or "consider_changes".
    pub label: String,
    /// One sentence, e.g. "Doing nothing is best: no signal passed validation, and the
    /// portfolio is within your limits."
    pub sentence: String,
}

/// Everything the Today view needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Today {
    /// When computed.
    pub as_of: DateTime<Utc>,
    /// The account view (value, free funds, invested, open result), when available.
    pub account: Option<crate::insights::AccountView>,
    /// The policy version used, and whether it is confirmed.
    pub policy: crate::policy::RiskPolicy,
    /// Overall stance.
    pub stance: Stance,
    /// Active suggestions (not dismissed or done), sorted.
    pub suggestions: Vec<Suggestion>,
    /// How many were hidden because they were dismissed or done.
    pub hidden: u32,
    /// The plans behind policy and idea suggestions, for the detail view.
    pub plans: Vec<(Mode, Plan)>,
}

/// A stable id for a suggestion: the first 16 hex characters of sha256 over the
/// kind, subject and evidence lines.
pub fn suggestion_id(kind: Kind, subject: &str, evidence: &[String]) -> String {
    let mut text = format!("{kind:?}\n{subject}\n");
    for line in evidence {
        text.push_str(line);
        text.push('\n');
    }
    crate::sha256_hex(text.as_bytes())[..16].to_string()
}
