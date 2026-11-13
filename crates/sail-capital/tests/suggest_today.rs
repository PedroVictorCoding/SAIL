//! Round 11: the risk policy, suggestion actions, and the Today rules.
use chrono::{Duration, TimeZone, Utc};
use rust_decimal::Decimal;
use sail_capital::contracts::{Currency, Money};
use sail_capital::insights::AccountView;
use sail_capital::optimize::{Breach, Change, Mode, Plan, Rule};
use sail_capital::policy::{PolicyStatus, RiskLimits, RiskPolicy};
use sail_capital::service::{CapitalService, ServiceError};
use sail_capital::suggest::build::{Facts, NewFiling, build, format_money};
use sail_capital::suggest::{Action, Kind, Severity, Tab};
use std::collections::BTreeSet;

const FILE: &[u8] = include_bytes!("fixtures/xtb/xtb_2026_layout.xlsx");

fn setup() -> (tempfile::TempDir, CapitalService, sail_capital::AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();
    let batch = svc
        .import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap();
    (dir, svc, batch.account)
}

fn eur(amount: i64) -> Money {
    Money {
        amount: Decimal::from(amount),
        currency: Currency::eur(),
    }
}

fn account_view() -> AccountView {
    AccountView {
        account_value: eur(20_000),
        free_funds: eur(6_000),
        invested: Some(eur(13_000)),
        open_result: Some(eur(1_000)),
    }
}

fn confirmed() -> RiskPolicy {
    RiskPolicy {
        version: 2,
        status: PolicyStatus::Confirmed,
        limits: RiskLimits::draft(),
        saved_at: Some(Utc::now()),
    }
}

fn empty_plan(mode: Mode) -> Plan {
    Plan {
        mode,
        changes: Vec::new(),
        cash_from_pct: 30.0,
        cash_to_pct: 30.0,
        breaches_before: Vec::new(),
        breaches_after: Vec::new(),
        feasible: true,
        expected_gain: 0.0,
        risk_change: 0.0,
        cost: 0.0,
        net_benefit: 0.0,
        note: String::new(),
    }
}

fn idle_cash() -> Breach {
    Breach {
        rule: Rule::IdleCash,
        symbol: None,
        actual_pct: 30.0,
        limit_pct: 15.0,
    }
}

fn facts(policy: RiskPolicy) -> Facts {
    let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
    Facts {
        now,
        account: Some(account_view()),
        policy,
        snapshot_as_of: Some(now - Duration::days(2)),
        variances: Vec::new(),
        unvalued: Vec::new(),
        prices_configured: true,
        without_prices: Vec::new(),
        stale_prices: Vec::new(),
        new_filings: Vec::new(),
        policy_plan: Some(empty_plan(Mode::Policy)),
        idea_plan: None,
        idea_horizon: 60,
        signals_passed: Vec::new(),
        combined_ic: 0.0,
    }
}

#[test]
fn the_policy_starts_as_the_builtin_draft_and_keeps_every_version() {
    let (_dir, svc, account) = setup();
    let view = svc.policy(&account).unwrap();
    assert_eq!(view.current.version, 0);
    assert_eq!(view.current.status, PolicyStatus::Draft);
    assert!(view.history.is_empty());

    let mut limits = RiskLimits::draft();
    limits.max_position_pct = Decimal::from(12);
    let v1 = svc
        .save_policy(&account, &limits, false, Utc::now())
        .unwrap();
    assert_eq!((v1.version, v1.status), (1, PolicyStatus::Draft));
    let v2 = svc
        .save_policy(&account, &limits, true, Utc::now())
        .unwrap();
    assert_eq!((v2.version, v2.status), (2, PolicyStatus::Confirmed));

    let view = svc.policy(&account).unwrap();
    assert_eq!(view.current, v2);
    assert_eq!(view.history.len(), 2);
    assert_eq!(view.history[1], v1);

    limits.cash_min_pct = Decimal::from(50);
    assert!(matches!(
        svc.save_policy(&account, &limits, true, Utc::now()),
        Err(ServiceError::BadRequest(_))
    ));
    assert_eq!(svc.policy(&account).unwrap().history.len(), 2);
}

#[test]
fn today_on_a_fresh_import_asks_to_review_limits_and_holds() {
    let (_dir, svc, account) = setup();
    let today = svc.today(&account, Utc::now()).unwrap();
    assert_eq!(today.policy.version, 0);
    assert!(today.account.is_some());
    let draft = today
        .suggestions
        .iter()
        .find(|s| s.title == "Your limits are a draft")
        .expect("draft notice");
    assert_eq!(draft.kind, Kind::Attention);
    assert_eq!(draft.tab, Tab::Settings);
    // Nothing is sized against a draft, and there are no ideas without a signal.
    assert!(today.suggestions.iter().all(|s| s.changes.is_empty()));
    assert!(today.suggestions.iter().all(|s| s.kind != Kind::Idea));
}

#[test]
fn dismissing_hides_a_suggestion_and_counts_it() {
    let (_dir, svc, account) = setup();
    let now = Utc::now();
    let before = svc.today(&account, now).unwrap();
    let first = before.suggestions[0].clone();
    let record = svc
        .suggestion_action(&account, &first.id, Action::Dismissed, now)
        .unwrap();
    assert_eq!(record.suggestion_id, first.id);
    let after = svc.today(&account, now).unwrap();
    assert!(after.suggestions.iter().all(|s| s.id != first.id));
    assert_eq!(after.hidden, before.hidden + 1);

    assert!(matches!(
        svc.suggestion_action(&account, "not-an-id", Action::Done, now),
        Err(ServiceError::BadRequest(_))
    ));
}

#[test]
fn a_breach_under_a_draft_is_unsized_and_points_to_the_limits() {
    let mut f = facts(RiskPolicy::builtin_draft());
    f.policy_plan.as_mut().unwrap().breaches_before = vec![idle_cash()];
    let today = build(f, &BTreeSet::new());
    assert_eq!(today.stance.label, "fix_limits");
    let s = today
        .suggestions
        .iter()
        .find(|s| s.kind == Kind::Policy)
        .unwrap();
    assert!(s.changes.is_empty());
    assert_eq!(s.tab, Tab::Settings);
    assert!(s.title.contains("30.0%") && s.title.contains("draft limit is 15%"));
    // 15 points of a 20,000 account sit idle above the limit.
    assert_eq!(
        s.if_nothing.as_deref(),
        Some("If nothing changes, 3,000 EUR stays idle above your limit.")
    );
}

#[test]
fn a_confirmed_policy_gives_one_sized_fix() {
    let mut f = facts(confirmed());
    let plan = f.policy_plan.as_mut().unwrap();
    plan.breaches_before = vec![idle_cash()];
    plan.changes = vec![Change {
        instrument_key: "sym:ABC.UK".into(),
        symbol: "ABC.UK".into(),
        from_pct: 20.0,
        to_pct: 35.0,
        amount: 3_000.0,
        reasons: vec![Rule::IdleCash],
    }];
    plan.cash_to_pct = 15.0;
    let today = build(f, &BTreeSet::new());
    let policy: Vec<_> = today
        .suggestions
        .iter()
        .filter(|s| s.kind == Kind::Policy)
        .collect();
    assert_eq!(policy.len(), 1);
    assert_eq!(policy[0].changes.len(), 1);
    assert_eq!(policy[0].changes[0].amount, 3_000.0);
    assert!(policy[0].title.starts_with("1 limit broken: 1 change"));
    assert_eq!(today.plans.len(), 1);
}

#[test]
fn ideas_need_a_validated_signal_and_a_positive_net_benefit() {
    let mut idea = empty_plan(Mode::Ideas);
    idea.changes = vec![Change {
        instrument_key: "sym:AAA.US".into(),
        symbol: "AAA.US".into(),
        from_pct: 5.0,
        to_pct: 8.0,
        amount: 600.0,
        reasons: vec![Rule::Signal],
    }];
    idea.expected_gain = 40.0;
    idea.cost = 3.0;
    idea.risk_change = 7.0;
    idea.net_benefit = 30.0;

    // No signal passed: no idea, and the stance says why.
    let mut f = facts(confirmed());
    f.idea_plan = Some(idea.clone());
    let today = build(f.clone(), &BTreeSet::new());
    assert!(today.suggestions.iter().all(|s| s.kind != Kind::Idea));
    assert_eq!(today.stance.label, "hold");
    assert!(today.stance.sentence.contains("no signal passed"));

    // A signal passed with a moderate combined IC.
    f.signals_passed = vec!["momentum_12_1 h60 t=2.6".into()];
    f.combined_ic = 0.06;
    let today = build(f.clone(), &BTreeSet::new());
    let s = today
        .suggestions
        .iter()
        .find(|s| s.kind == Kind::Idea)
        .unwrap();
    assert_eq!(s.confidence.as_deref(), Some("medium"));
    assert_eq!(s.changes.len(), 1);
    assert!(s.why.contains("momentum_12_1 h60 t=2.6"));
    assert!(s.if_nothing.as_deref().unwrap().contains("30 EUR"));
    assert_eq!(today.stance.label, "consider_changes");

    // Costs and risk outweigh the gain: no idea.
    f.idea_plan.as_mut().unwrap().net_benefit = -1.0;
    let today = build(f, &BTreeSet::new());
    assert!(today.suggestions.iter().all(|s| s.kind != Kind::Idea));
    assert!(
        today
            .stance
            .sentence
            .contains("don't beat their risk and costs")
    );
}

#[test]
fn attention_rules_fire_on_their_facts_and_sort_by_severity() {
    let mut f = facts(confirmed());
    f.snapshot_as_of = Some(f.now - Duration::days(60));
    f.variances = vec!["cash EUR".into()];
    f.without_prices = vec!["AAA.US".into(), "BBB.DE".into()];
    f.new_filings = vec![NewFiling {
        symbol: "AAA Corp".into(),
        form: "8-K".into(),
        accession: "0000000000-26-000001".into(),
    }];
    let today = build(f, &BTreeSet::new());
    let titles: Vec<&str> = today.suggestions.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles[0], "Your XTB data is 60 days old");
    assert_eq!(today.suggestions[0].severity, Severity::High);
    assert!(titles.contains(&"1 figure differs from XTB"));
    assert!(titles.contains(&"2 holdings have no prices"));
    assert!(titles.contains(&"1 new SEC filing on your holdings"));
    let severities: Vec<Severity> = today.suggestions.iter().map(|s| s.severity).collect();
    let mut sorted = severities.clone();
    sorted.sort();
    assert_eq!(severities, sorted);
}

#[test]
fn ids_are_stable_and_follow_the_evidence() {
    let f = facts(RiskPolicy::builtin_draft());
    let a = build(f.clone(), &BTreeSet::new());
    let b = build(f.clone(), &BTreeSet::new());
    assert_eq!(
        a.suggestions.iter().map(|s| &s.id).collect::<Vec<_>>(),
        b.suggestions.iter().map(|s| &s.id).collect::<Vec<_>>()
    );
    let mut handled = BTreeSet::new();
    handled.insert(a.suggestions[0].id.clone());
    let c = build(f, &handled);
    assert_eq!(c.hidden, 1);
    assert_eq!(c.suggestions.len(), a.suggestions.len() - 1);
}

#[test]
fn money_is_whole_units_with_separators() {
    assert_eq!(format_money(1234567.4, "EUR"), "1,234,567 EUR");
    assert_eq!(format_money(-999.6, "EUR"), "-1,000 EUR");
    assert_eq!(format_money(12.0, ""), "12");
}
