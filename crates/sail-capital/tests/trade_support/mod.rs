#![allow(dead_code)]

use chrono::{Days, NaiveDate};
use sail_capital::trade::{
    AssetClass, Bar, Costs, Evidence, Instrument, SetupId, Side, SignalCard, Verdict,
};

pub fn base_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2018, 1, 2).expect("a date")
}

pub fn date_at(index: usize) -> NaiveDate {
    base_date() + Days::new(index as u64)
}

pub fn bar(index: usize, close: f64, half_range: f64) -> Bar {
    Bar {
        session: date_at(index),
        open: close,
        high: close + half_range,
        low: close - half_range,
        close,
        volume: None,
        dividend: None,
    }
}

pub fn bars_from_closes(closes: &[f64], half_range: f64) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(index, close)| bar(index, *close, half_range))
        .collect()
}

pub fn bars_with_prior_open(closes: &[f64], half_range: f64) -> Vec<Bar> {
    let mut previous = closes.first().copied().unwrap_or_default();
    let mut bars = Vec::with_capacity(closes.len());
    for (index, close) in closes.iter().copied().enumerate() {
        let open = previous;
        previous = close;
        bars.push(Bar {
            session: date_at(index),
            open,
            high: open.max(close) + half_range,
            low: open.min(close) - half_range,
            close,
            volume: None,
            dividend: None,
        });
    }
    bars
}

pub fn instrument(
    key: &str,
    symbol: &str,
    class: AssetClass,
    foreign: bool,
    bars: Vec<Bar>,
) -> Instrument {
    Instrument {
        key: key.into(),
        symbol: symbol.into(),
        class,
        foreign_currency: foreign,
        bars,
        earnings: Vec::new(),
        evidence_only: false,
        membership: Vec::new(),
        context: Default::default(),
    }
}

pub struct Lcg {
    state: u64,
}

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    pub fn uniform(&mut self) -> f64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.state >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn normal(&mut self) -> f64 {
        let first = self.uniform().max(f64::MIN_POSITIVE);
        let second = self.uniform();
        (-2.0 * first.ln()).sqrt() * (std::f64::consts::TAU * second).cos()
    }
}

pub fn random_walk(length: usize, drift: f64, sigma: f64, seed: u64) -> (Vec<f64>, Vec<f64>) {
    let mut rng = Lcg::new(seed);
    let mut close = 100.0;
    let mut closes = Vec::with_capacity(length);
    let mut ranges = Vec::with_capacity(length);
    for index in 0..length {
        if index > 0 {
            close *= (drift + sigma * rng.normal()).exp();
        }
        closes.push(close);
        ranges.push(close * 0.01f64.max(sigma));
    }
    (closes, ranges)
}

pub fn planted_closes(length: usize, seed: u64) -> Vec<f64> {
    let mut rng = Lcg::new(seed);
    let mut close = 80.0;
    let mut closes = Vec::with_capacity(length);
    for _ in 0..220 {
        close += 0.1;
        closes.push(close);
    }
    while closes.len() < length {
        for _ in 0..5 {
            close += 0.8;
            closes.push(close);
        }
        for _ in 0..3 {
            close -= 0.6;
            closes.push(close);
        }
        for _ in 0..15 {
            close += 0.8 + rng.normal() * 0.02;
            closes.push(close);
        }
    }
    closes.truncate(length);
    closes
}

pub fn zero_costs() -> Costs {
    Costs {
        round_trip: 0.0,
        fx_conversion: 0.0,
        cfd_daily_financing: 0.0,
    }
}

pub fn dummy_evidence(setup: SetupId, side: Side) -> Evidence {
    Evidence {
        setup,
        side,
        trades: 0,
        hit_target1: 0.0,
        hit_target1_ci: (0.0, 0.0),
        expectancy_r: 0.0,
        expectancy_ci: (0.0, 0.0),
        profit_factor: 0.0,
        max_drawdown_r: 0.0,
        mean_sessions: 0.0,
    }
}

pub fn dummy_verdict(setup: SetupId, side: Side) -> Verdict {
    Verdict {
        group: "stocks".into(),
        selection: dummy_evidence(setup, side),
        holdout: dummy_evidence(setup, side),
        publishes: false,
        candidate: false,
        reason: String::new(),
        judgement: None,
        condition: Default::default(),
    }
}

pub fn card(
    side: Side,
    setup: SetupId,
    entry: f64,
    stop: f64,
    targets: [f64; 3],
    max_sessions: u32,
    needs_cfd: bool,
) -> SignalCard {
    SignalCard {
        id: "test".into(),
        key: "test".into(),
        symbol: "TST".into(),
        class: AssetClass::Stock,
        side,
        setup,
        signal_session: date_at(0),
        entry_zone: (entry, entry),
        stop,
        targets,
        max_sessions,
        size: None,
        risk_pct: 0.5,
        needs_cfd,
        verdict: dummy_verdict(setup, side),
        score: 0.0,
        relation: None,
        warnings: Vec::new(),
        tier: Default::default(),
        expectations: None,
        sessions_to_verdict: None,
    }
}
