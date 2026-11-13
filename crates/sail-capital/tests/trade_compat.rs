//! Stored runs and cards from before round 19 still decode (rule 8).

use chrono::NaiveDate;
use sail_capital::store::GradedCard;
use sail_capital::trade::{SignalDay, Tier};
use serde_json::json;

fn evidence() -> serde_json::Value {
    json!({
        "setup": "three_day_reversal",
        "side": "long",
        "trades": 100,
        "hit_target1": 0.5,
        "hit_target1_ci": [0.4, 0.6],
        "expectancy_r": 0.1,
        "expectancy_ci": [0.02, 0.18],
        "profit_factor": 1.2,
        "max_drawdown_r": 5.0,
        "mean_sessions": 5.0
    })
}

fn verdict() -> serde_json::Value {
    json!({
        "group": "stocks",
        "selection": evidence(),
        "holdout": evidence(),
        "publishes": true,
        "reason": "Publishes: +0.10R a trade over 100 trades"
    })
}

fn card() -> serde_json::Value {
    json!({
        "id": "abc",
        "key": "sym:AAPL.US",
        "symbol": "AAPL.US",
        "class": "stock",
        "side": "long",
        "setup": "three_day_reversal",
        "signal_session": "2025-01-02",
        "entry_zone": [99.5, 100.5],
        "stop": 95.0,
        "targets": [105.0, 110.0, 115.0],
        "max_sessions": 5,
        "size": null,
        "risk_pct": 0.5,
        "needs_cfd": false,
        "verdict": verdict(),
        "score": 0.02,
        "relation": null,
        "warnings": []
    })
}

#[test]
fn a_pre_round_19_signal_day_decodes() {
    let text = json!({
        "session": "2025-01-02",
        "cards": [card()],
        "scoreboard": [verdict()],
        "suppressed": [],
        "notes": ["old"]
    })
    .to_string();
    let day: SignalDay = serde_json::from_str(&text).expect("a pre-round-19 run");
    assert_eq!(day.session, NaiveDate::from_ymd_opt(2025, 1, 2).unwrap());
    assert!(day.candidates.is_empty());
    assert!(day.run_stats.is_none());
    assert_eq!(day.cards.len(), 1);
    assert_eq!(day.cards[0].tier, Tier::Proven);
    assert!(day.cards[0].verdict.judgement.is_none());
    assert!(day.scoreboard[0].judgement.is_none());
}

#[test]
fn a_pre_round_19_graded_card_decodes() {
    let text = json!({
        "card": card(),
        "published_at": "2025-01-03T00:00:00Z",
        "graded": null,
        "status": "open"
    })
    .to_string();
    let graded: GradedCard = serde_json::from_str(&text).expect("a pre-round-19 card");
    assert_eq!(graded.status, "open");
    assert!(graded.graded.is_none());
    assert_eq!(graded.card.tier, Tier::Proven);
    assert!(graded.card.verdict.judgement.is_none());
}
