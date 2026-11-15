//! Round 24 end to end for the trade side: dividends in the replay, the regime
//! conditioning, the two new setups' cells and portfolio heat, through
//! `signal_day_with` as the service calls it. Red until Kilo's slice (K1–K4) lands.

mod trade_support;

use std::collections::HashMap;

use sail_capital::trade::heat::{CorrInputs, HeatLimits, OpenCard};
use sail_capital::trade::regime::{Condition, US_REFERENCE};
use sail_capital::trade::{
    AssetClass, Bar, Costs, Instrument, InstrumentContext, Region, RunInputs, SetupId, Side,
    adjust, replay, signal_day_with,
};
use trade_support::{bars_with_prior_open, instrument, random_walk};

/// A US stock on a random walk with volume every session and a 1.5% dividend each
/// quarter.
fn payer(key: &str, seed: u64) -> Instrument {
    let (closes, _) = random_walk(2000, 0.0002, 0.012, seed);
    let mut bars: Vec<Bar> = bars_with_prior_open(&closes, 0.4);
    for (i, bar) in bars.iter_mut().enumerate() {
        bar.volume = Some(1_000_000.0 * (1.0 + (i % 7) as f64 / 10.0));
        if i > 0 && i % 63 == 0 {
            bar.dividend = Some(bar.close * 0.015);
        }
    }
    let mut inst = instrument(key, &format!("{key}.US"), AssetClass::Stock, false, bars);
    inst.context = InstrumentContext {
        region: Region::Us,
        earnings_events: Vec::new(),
        withholding: 0.15,
    };
    inst
}

fn reference() -> Vec<Bar> {
    let (closes, _) = random_walk(2000, 0.0003, 0.01, 77);
    bars_with_prior_open(&closes, 0.3)
}

#[test]
fn adjusted_bars_scale_history_down_before_each_dividend() {
    let inst = payer("A", 1);
    let adjusted = adjust(&inst.bars);
    assert_eq!(adjusted.bars.len(), inst.bars.len());
    assert_eq!(adjusted.factor.last().copied(), Some(1.0));
    assert!(adjusted.factor[0] < 0.75, "about 31 dividends of 1.5% each");
    for w in adjusted.factor.windows(2) {
        assert!(w[0] <= w[1] + 1e-12);
    }
}

#[test]
fn dividends_enter_the_replayed_r() {
    let inst = payer("A", 2);
    let trades = replay(&inst, Costs::default_for(AssetClass::Stock));
    let with_dividend: Vec<_> = trades.iter().filter(|t| t.dividend_r != 0.0).collect();
    assert!(!with_dividend.is_empty(), "no trade held through an ex-date");
    for t in &with_dividend {
        match t.side {
            Side::Long => assert!(t.dividend_r > 0.0),
            Side::Short => assert!(t.dividend_r < 0.0),
        }
    }
}

#[test]
fn the_run_conditions_on_the_regime_and_caps_heat() {
    let universe: Vec<Instrument> = (0..8).map(|i| payer(&format!("S{i}"), 100 + i)).collect();
    let references = HashMap::from([(US_REFERENCE.to_string(), reference())]);
    let open = vec![OpenCard {
        key: "S0".into(),
        side: Side::Long,
        risk_pct: 2.9,
    }];
    let day = signal_day_with(&RunInputs {
        universe: &universe,
        replays: &HashMap::new(),
        live: &[],
        account_value: Some(100_000.0),
        risk_pct: 0.5,
        fx_to_account: &|_| Some(1.0),
        held: &|_| None,
        references: &references,
        open_cards: &open,
        correlations: &CorrInputs::default(),
        heat_limits: HeatLimits::default(),
    });
    // 3 groups × 8 setups × 2 sides × 2 conditions.
    assert_eq!(day.scoreboard.len(), 3 * SetupId::ALL.len() * 2 * 2);
    assert!(
        day.scoreboard
            .iter()
            .any(|v| v.condition == Condition::AgreesCalm)
    );
    assert!(day.regime.contains_key(US_REFERENCE));
    let heat = day.heat.expect("a heat summary");
    assert!(heat.total_risk_pct <= heat.limit_pct + 1e-9);
    assert!(heat.total_risk_pct >= 2.9);
}
