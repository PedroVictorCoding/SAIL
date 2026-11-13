//! Building the Today view from facts. Pure: the service gathers the facts, this decides
//! what to say. Each rule is plain and fixed, and none of them uses a model.
use super::{Kind, Severity, SizedChange, Stance, Suggestion, Tab, Today, suggestion_id};
use crate::insights::AccountView;
use crate::optimize::{Breach, Mode, Plan, Rule};
use crate::policy::RiskPolicy;
use chrono::{DateTime, NaiveDate, Utc};
use std::collections::BTreeSet;

/// XTB data older than this many days is flagged.
pub const STALE_SNAPSHOT_DAYS: i64 = 14;
/// ... and flagged as important past this many.
pub const VERY_STALE_SNAPSHOT_DAYS: i64 = 45;
/// Prices whose last session is older than this many days are flagged.
pub const STALE_PRICE_DAYS: i64 = 7;
/// Filings first seen within this many days count as new.
pub const NEW_FILING_DAYS: i64 = 14;

/// A filing SAIL first saw recently, on a held issuer.
#[derive(Clone, Debug, PartialEq)]
pub struct NewFiling {
    /// Broker symbol.
    pub symbol: String,
    /// Form type, e.g. "8-K".
    pub form: String,
    /// Accession number.
    pub accession: String,
}

/// Everything the rules look at.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    /// Now.
    pub now: DateTime<Utc>,
    /// The account view, when there is a portfolio.
    pub account: Option<AccountView>,
    /// The policy in force.
    pub policy: RiskPolicy,
    /// When XTB's latest snapshot was taken.
    pub snapshot_as_of: Option<DateTime<Utc>>,
    /// Subjects where the ledger and XTB disagree.
    pub variances: Vec<String>,
    /// Held positions XTB gave no value for.
    pub unvalued: Vec<String>,
    /// Whether price fetching is on.
    pub prices_configured: bool,
    /// Held instruments with no stored prices.
    pub without_prices: Vec<String>,
    /// Held instruments whose latest stored session is old: (symbol, last session).
    pub stale_prices: Vec<(String, NaiveDate)>,
    /// Filings first seen recently.
    pub new_filings: Vec<NewFiling>,
    /// The policy-mode plan (always computed when there is a portfolio).
    pub policy_plan: Option<Plan>,
    /// The ideas-mode plan (only with a confirmed policy and a surviving signal).
    pub idea_plan: Option<Plan>,
    /// Horizon of the ideas, in sessions.
    pub idea_horizon: u32,
    /// Signals that passed validation at that horizon, e.g. `"momentum_12_1 h60 t=2.4"`.
    pub signals_passed: Vec<String>,
    /// Combined IC at that horizon (0 when nothing passed).
    pub combined_ic: f64,
}

/// Builds Today. Suggestions whose id is in `handled` are hidden and counted.
pub fn build(facts: Facts, handled: &BTreeSet<String>) -> Today {
    let currency = facts
        .account
        .as_ref()
        .map(|a| a.account_value.currency.as_str().to_string())
        .unwrap_or_default();
    let account_value = facts
        .account
        .as_ref()
        .and_then(|a| decimal_f64(a.account_value.amount))
        .unwrap_or(0.0);
    let money = |x: f64| format_money(x, &currency);

    let mut all: Vec<Suggestion> = Vec::new();
    attention(&facts, &mut all);
    let breaches = facts
        .policy_plan
        .as_ref()
        .map(|p| p.breaches_before.clone())
        .unwrap_or_default();
    policy_suggestions(&facts, &breaches, account_value, &money, &mut all);
    let idea = idea_suggestion(&facts, &money);
    let idea_exists = idea.is_some();
    all.extend(idea);

    all.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.kind.cmp(&b.kind)));
    let stance = stance(&facts, !breaches.is_empty(), idea_exists);
    let before = all.len();
    let suggestions: Vec<Suggestion> = all
        .into_iter()
        .filter(|s| !handled.contains(&s.id))
        .collect();
    let hidden = (before - suggestions.len()) as u32;
    let mut plans = Vec::new();
    if let Some(p) = facts.policy_plan.clone() {
        plans.push((Mode::Policy, p));
    }
    if let Some(p) = facts.idea_plan.clone() {
        plans.push((Mode::Ideas, p));
    }
    Today {
        as_of: facts.now,
        account: facts.account,
        policy: facts.policy,
        stance,
        suggestions,
        hidden,
        plans,
    }
}

fn attention(facts: &Facts, out: &mut Vec<Suggestion>) {
    let push = |out: &mut Vec<Suggestion>,
                severity: Severity,
                subject: &str,
                title: String,
                why: String,
                evidence: Vec<String>,
                tab: Tab| {
        out.push(Suggestion {
            id: suggestion_id(Kind::Attention, subject, &evidence),
            kind: Kind::Attention,
            severity,
            title,
            why,
            if_nothing: None,
            changes: Vec::new(),
            rules: Vec::new(),
            confidence: None,
            evidence,
            tab,
        });
    };

    if facts.account.is_none() {
        push(
            out,
            Severity::High,
            "no-portfolio",
            "Upload an XTB export to start".into(),
            "SAIL has no positions for this account yet. Upload the account export from XTB with Upload report.".into(),
            vec![],
            Tab::Settings,
        );
        return;
    }

    if !facts.policy.is_confirmed() {
        push(
            out,
            Severity::Medium,
            "policy-draft",
            "Your limits are a draft".into(),
            "SAIL sizes proposals only against limits you confirmed. Review the starting values in Settings → Limits and confirm them.".into(),
            vec![format!("policy v{} draft", facts.policy.version)],
            Tab::Settings,
        );
    }

    if let Some(at) = facts.snapshot_as_of {
        let days = (facts.now - at).num_days();
        if days > STALE_SNAPSHOT_DAYS {
            push(
                out,
                if days > VERY_STALE_SNAPSHOT_DAYS {
                    Severity::High
                } else {
                    Severity::Medium
                },
                "snapshot-age",
                format!("Your XTB data is {days} days old"),
                "Positions, cash and every suggestion are as of the last upload. Upload a fresh export from XTB to bring them up to date.".into(),
                vec![format!("snapshot {}", at.date_naive())],
                Tab::Settings,
            );
        }
    }

    if !facts.variances.is_empty() {
        let n = facts.variances.len();
        push(
            out,
            Severity::Medium,
            "variances",
            format!(
                "{n} figure{} differ{} from XTB",
                plural(n),
                if n == 1 { "s" } else { "" }
            ),
            format!(
                "The ledger and XTB's own figures disagree on {}. Check them in Holdings → Reconciliation before relying on totals.",
                list(&facts.variances, 3)
            ),
            facts.variances.clone(),
            Tab::Portfolio,
        );
    }

    if !facts.prices_configured {
        push(
            out,
            Severity::Low,
            "prices-off",
            "Prices aren't being fetched".into(),
            "Forecasts, volatility limits and ideas need daily prices. Turn on fetching or upload price files in Settings → Prices.".into(),
            vec![],
            Tab::Settings,
        );
    }

    if !facts.without_prices.is_empty() {
        let n = facts.without_prices.len();
        push(
            out,
            Severity::Medium,
            "without-prices",
            format!("{n} holding{} {} no prices", plural(n), have(n)),
            format!(
                "{} can't be checked for volatility or included in ideas. Fetch prices, or upload a price file for {}.",
                list(&facts.without_prices, 4),
                if n == 1 { "it" } else { "them" }
            ),
            facts.without_prices.clone(),
            Tab::Settings,
        );
    }

    if !facts.stale_prices.is_empty() {
        let n = facts.stale_prices.len();
        let symbols: Vec<String> = facts.stale_prices.iter().map(|(s, _)| s.clone()).collect();
        push(
            out,
            Severity::Low,
            "stale-prices",
            format!("Prices are out of date for {n} holding{}", plural(n)),
            format!(
                "The latest stored price for {} is more than {STALE_PRICE_DAYS} days old. Fetch prices again.",
                list(&symbols, 4)
            ),
            facts
                .stale_prices
                .iter()
                .map(|(s, d)| format!("{s} {d}"))
                .collect(),
            Tab::Settings,
        );
    }

    if !facts.new_filings.is_empty() {
        let n = facts.new_filings.len();
        let symbols: BTreeSet<&str> = facts
            .new_filings
            .iter()
            .map(|f| f.symbol.as_str())
            .collect();
        let symbols: Vec<String> = symbols.into_iter().map(str::to_string).collect();
        push(
            out,
            Severity::Low,
            "new-filings",
            format!("{n} new SEC filing{} on your holdings", plural(n)),
            format!(
                "New filings from {} in the last {NEW_FILING_DAYS} days. Read them in Research.",
                list(&symbols, 4)
            ),
            facts
                .new_filings
                .iter()
                .map(|f| format!("filing {} {}", f.form, f.accession))
                .collect(),
            Tab::Portfolio,
        );
    }

    if !facts.unvalued.is_empty() {
        let n = facts.unvalued.len();
        push(
            out,
            Severity::Low,
            "unvalued",
            format!("{n} position{} {} no value from XTB", plural(n), have(n)),
            format!(
                "{} {} left out of shares and limits until the next export values {}.",
                list(&facts.unvalued, 4),
                if n == 1 { "is" } else { "are" },
                if n == 1 { "it" } else { "them" }
            ),
            facts.unvalued.clone(),
            Tab::Portfolio,
        );
    }
}

fn breach_title(b: &Breach, draft: bool) -> String {
    let limit = if draft {
        "the draft limit"
    } else {
        "your limit"
    };
    match b.rule {
        Rule::MaxPosition => format!(
            "{} is {:.1}% of the account; {limit} is {:.0}%",
            b.symbol.as_deref().unwrap_or("A position"),
            b.actual_pct,
            b.limit_pct
        ),
        Rule::HighVolShare => format!(
            "High-volatility holdings are {:.1}% of the account; {limit} is {:.0}%",
            b.actual_pct, b.limit_pct
        ),
        Rule::CashReserve => format!(
            "Free funds are {:.1}%, below {limit} of {:.0}%",
            b.actual_pct, b.limit_pct
        ),
        Rule::IdleCash => format!(
            "Free funds are {:.1}% of the account; {limit} is {:.0}%",
            b.actual_pct, b.limit_pct
        ),
        Rule::Signal => "A signal favours a change".into(),
    }
}

fn breach_if_nothing(b: &Breach, account_value: f64, money: &dyn Fn(f64) -> String) -> String {
    let share = |pct: f64| account_value * pct / 100.0;
    match b.rule {
        Rule::MaxPosition => format!(
            "If nothing changes, a 30% fall in {} alone would cost about {}.",
            b.symbol.as_deref().unwrap_or("it"),
            money(share(b.actual_pct) * 0.3)
        ),
        Rule::HighVolShare => format!(
            "If nothing changes, a 40% fall across them would cost about {}.",
            money(share(b.actual_pct) * 0.4)
        ),
        Rule::CashReserve => format!(
            "If nothing changes, your reserve is {} short of your limit.",
            money(share(b.limit_pct - b.actual_pct))
        ),
        Rule::IdleCash => format!(
            "If nothing changes, {} stays idle above your limit.",
            money(share(b.actual_pct - b.limit_pct))
        ),
        Rule::Signal => String::new(),
    }
}

fn breach_severity(b: &Breach) -> Severity {
    let far = match b.rule {
        Rule::CashReserve => b.actual_pct < b.limit_pct * 0.5,
        _ => b.actual_pct > b.limit_pct * 1.5,
    };
    if far {
        Severity::High
    } else {
        Severity::Medium
    }
}

fn breach_evidence(b: &Breach, policy: &RiskPolicy) -> Vec<String> {
    vec![
        format!("policy v{}", policy.version),
        format!(
            "{:?} {} {:.0}%",
            b.rule,
            b.symbol.as_deref().unwrap_or("-"),
            b.actual_pct
        ),
    ]
}

fn sized(plan: &Plan) -> Vec<SizedChange> {
    plan.changes
        .iter()
        .map(|c| SizedChange {
            symbol: c.symbol.clone(),
            from_pct: c.from_pct,
            to_pct: c.to_pct,
            amount: c.amount,
        })
        .collect()
}

fn policy_suggestions(
    facts: &Facts,
    breaches: &[Breach],
    account_value: f64,
    money: &dyn Fn(f64) -> String,
    out: &mut Vec<Suggestion>,
) {
    if breaches.is_empty() {
        return;
    }
    let draft = !facts.policy.is_confirmed();
    let plan = facts.policy_plan.as_ref().filter(|p| !p.changes.is_empty());
    match (draft, plan) {
        (false, Some(plan)) => {
            let n = breaches.len();
            let mut why: Vec<String> = breaches.iter().map(|b| breach_title(b, false)).collect();
            why.push(plan.note.clone());
            if !plan.feasible {
                why.push(format!(
                    "They can't fix everything within your turnover limit: {} remain.",
                    plan.breaches_after.len()
                ));
            }
            let mut evidence = vec![format!("policy v{}", facts.policy.version)];
            evidence.extend(plan.changes.iter().map(|c| {
                format!(
                    "{} {}",
                    if c.amount >= 0.0 { "add" } else { "reduce" },
                    c.symbol
                )
            }));
            out.push(Suggestion {
                id: suggestion_id(Kind::Policy, "policy-plan", &evidence),
                kind: Kind::Policy,
                severity: breaches
                    .iter()
                    .map(breach_severity)
                    .min()
                    .unwrap_or(Severity::Medium),
                title: format!(
                    "{n} limit{} broken: {} change{} would fix {}",
                    plural(n),
                    plan.changes.len(),
                    plural(plan.changes.len()),
                    if n == 1 { "it" } else { "them" }
                ),
                why: why.join(" "),
                if_nothing: Some(breach_if_nothing(&breaches[0], account_value, money)),
                changes: sized(plan),
                rules: breaches.iter().map(|b| b.rule).collect(),
                confidence: None,
                evidence,
                tab: Tab::Today,
            });
        }
        _ => {
            for b in breaches {
                let evidence = breach_evidence(b, &facts.policy);
                let why = if draft {
                    "Confirm your limits in Settings → Limits to see the smallest change that fixes this."
                } else {
                    "No change above your smallest trade size fixes this on its own."
                };
                out.push(Suggestion {
                    id: suggestion_id(
                        Kind::Policy,
                        b.symbol.as_deref().unwrap_or("account"),
                        &evidence,
                    ),
                    kind: Kind::Policy,
                    severity: breach_severity(b),
                    title: breach_title(b, draft),
                    why: why.into(),
                    if_nothing: Some(breach_if_nothing(b, account_value, money)),
                    changes: Vec::new(),
                    rules: vec![b.rule],
                    confidence: None,
                    evidence,
                    tab: if draft { Tab::Settings } else { Tab::Portfolio },
                });
            }
        }
    }
}

/// "low", "medium" or "high" from the combined IC.
pub fn confidence(combined_ic: f64) -> &'static str {
    if combined_ic >= 0.10 {
        "high"
    } else if combined_ic >= 0.05 {
        "medium"
    } else {
        "low"
    }
}

fn idea_suggestion(facts: &Facts, money: &dyn Fn(f64) -> String) -> Option<Suggestion> {
    let plan = facts.idea_plan.as_ref()?;
    if plan.changes.is_empty() || plan.net_benefit <= 0.0 || facts.combined_ic <= 0.0 {
        return None;
    }
    let confidence = confidence(facts.combined_ic);
    let k = plan.changes.len();
    let weeks = (facts.idea_horizon as f64 / 5.0).round() as u32;
    let mut evidence = facts.signals_passed.clone();
    evidence.extend(plan.changes.iter().map(|c| {
        format!(
            "{} {}",
            if c.amount >= 0.0 { "add" } else { "reduce" },
            c.symbol
        )
    }));
    Some(Suggestion {
        id: suggestion_id(Kind::Idea, "ideas", &evidence),
        kind: Kind::Idea,
        severity: if confidence == "low" {
            Severity::Low
        } else {
            Severity::Medium
        },
        title: format!(
            "Idea: {k} change{} with an expected net benefit of {} over about {weeks} weeks",
            plural(k),
            money(plan.net_benefit)
        ),
        why: format!(
            "Signals that passed out-of-sample checks: {}. Their combined information coefficient is {:.2}, so most of the time the outcome will differ from the expectation. Expected gain {}, cost {}, risk change {}.",
            facts.signals_passed.join(", "),
            facts.combined_ic,
            money(plan.expected_gain),
            money(plan.cost),
            money(plan.risk_change)
        ),
        if_nothing: Some(format!(
            "Doing nothing costs nothing and gives up an expected {} over the period. That is an average, not a promise.",
            money(plan.net_benefit)
        )),
        changes: sized(plan),
        rules: vec![Rule::Signal],
        confidence: Some(confidence.into()),
        evidence,
        tab: Tab::Record,
    })
}

fn stance(facts: &Facts, breached: bool, idea: bool) -> Stance {
    let draft = !facts.policy.is_confirmed();
    if facts.account.is_none() {
        return Stance {
            label: "hold".into(),
            sentence: "Upload an XTB export to see where the account stands.".into(),
        };
    }
    if breached {
        return Stance {
            label: "fix_limits".into(),
            sentence: if draft {
                "Some holdings break your draft limits. Confirm the limits to get sized fixes."
                    .into()
            } else {
                "Fix your limits first: the account is outside them.".into()
            },
        };
    }
    if idea {
        return Stance {
            label: "consider_changes".into(),
            sentence: "Validated signals favour some changes. They are ideas: compare each with doing nothing.".into(),
        };
    }
    let within = if draft {
        "within your draft limits"
    } else {
        "within your limits"
    };
    let reason = if facts.signals_passed.is_empty() {
        "no signal passed out-of-sample checks"
    } else if draft {
        "ideas wait for confirmed limits"
    } else {
        "the changes signals favour don't beat their risk and costs"
    };
    Stance {
        label: "hold".into(),
        sentence: format!("Doing nothing is best for now: {reason}, and the account is {within}."),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn have(n: usize) -> &'static str {
    if n == 1 { "has" } else { "have" }
}

fn list(items: &[String], max: usize) -> String {
    let shown: Vec<&str> = items.iter().take(max).map(String::as_str).collect();
    let more = items.len().saturating_sub(max);
    let base = shown.join(", ");
    if more > 0 {
        format!("{base} and {more} more")
    } else {
        base
    }
}

fn decimal_f64(d: rust_decimal::Decimal) -> Option<f64> {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64()
}

/// `1,234 EUR`: whole units with thousands separators.
pub fn format_money(x: f64, currency: &str) -> String {
    let rounded = x.round() as i64;
    let digits = rounded.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let sign = if rounded < 0 { "-" } else { "" };
    if currency.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped} {currency}")
    }
}
