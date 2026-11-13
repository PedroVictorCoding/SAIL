//! The signal board: one row per instrument (holdings, then the watchlist's stocks and
//! ETFs) with everything SAIL can say about it today, side by side.
//!
//! A row gathers four kinds of evidence, each labelled with how far it can be trusted:
//! - **Readings**: the four price signals (`signals::SignalId::PRICE`) in plain units
//!   (e.g. "up 34% over the past year, skipping the last month") and as a percentile
//!   across the board, where 100 is the most favourable. They describe the past.
//! - **Combined signal**: the validated combination from `signals`, at 1 and 3 months.
//!   It is non-zero only when a signal passed out-of-sample checks; then `alpha` is the
//!   expected return relative to the rest of the board.
//! - **Outlook**: the blended price outlook (`outlook`) at 1 and 3 months: the median
//!   return, the 5–95% range and the chance of ending higher. The outlook's own track
//!   record is reported once, for the whole board.
//! - **Setup**: a trade setup that fired on the latest session, whether it published
//!   (its setup earned a record) or not.
//!
//! The **read** sums these up in one word per row:
//! - with a validated combination, its sign: `favoured` when the combined score is at
//!   least [`LEAN_SCORE`] standard deviations above the board, `disfavoured` when that far
//!   below, `neutral` between. `proven` is true;
//! - otherwise, the agreement of the readings: a reading at or above the
//!   [`AGREE_HIGH`] percentile counts for, at or below [`AGREE_LOW`] against. Two or more
//!   net counts give `favoured` or `disfavoured`; less is `mixed`. `proven` is false,
//!   and the sentence says it is a description, not a prediction.
//!
//! **Purity.** No I/O and no clock.
use crate::signals::{CombinedScore, SignalId, SignalReport};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Combined score, in standard deviations, past which a validated read leans.
pub const LEAN_SCORE: f64 = 0.5;
/// Percentile at or above which a reading counts in favour.
pub const AGREE_HIGH: f64 = 67.0;
/// Percentile at or below which a reading counts against.
pub const AGREE_LOW: f64 = 33.0;
/// Horizons on the board, in sessions: signal horizon and outlook horizon.
pub const BOARD_HORIZONS: [(u32, u32); 2] = [(20, 21), (60, 63)];

/// What an instrument is, before its evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardMeta {
    /// Store key of the price series.
    pub key: String,
    /// XTB symbol.
    pub symbol: String,
    /// Display name.
    pub name: String,
    /// `stock` or `etf`.
    pub class: String,
    /// The owner holds it.
    pub held: bool,
}

/// One price signal for one instrument.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// The signal.
    pub signal: SignalId,
    /// Raw value (see `signals::SignalId`).
    pub raw: f64,
    /// Plain description, e.g. "Up 34% over the past year, skipping the last month".
    pub text: String,
    /// Percentile across the board, 0–100, where 100 is the most favourable.
    pub percentile: f64,
}

/// The validated combination at one horizon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Combined {
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Combined score (standard deviations across the board).
    pub score: f64,
    /// Expected log return relative to the board; 0 when nothing passed.
    pub alpha: f64,
}

/// The blended outlook at one horizon, in returns from the latest close.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlookSummary {
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Median simple return.
    pub median: f64,
    /// 5% simple return.
    pub low: f64,
    /// 95% simple return.
    pub high: f64,
    /// Chance of ending above the latest close, read from the fan's quantiles.
    pub p_up: f64,
}

/// A trade setup that fired on the latest session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetupView {
    /// Setup.
    pub setup: crate::trade::SetupId,
    /// Side.
    pub side: crate::trade::Side,
    /// The setup earned a record and the card published.
    pub published: bool,
}

/// One word for the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lean {
    /// The evidence favours it against the rest of the board.
    Favoured,
    /// The evidence is against it.
    Disfavoured,
    /// Validated signals see nothing unusual.
    Neutral,
    /// Unvalidated readings disagree.
    Mixed,
    /// Too little price history.
    NoData,
}

/// The row's read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Read {
    /// Lean.
    pub lean: Lean,
    /// Rests on signals that passed out-of-sample checks.
    pub proven: bool,
    /// Readings counting for.
    pub for_count: u32,
    /// Readings counting against.
    pub against_count: u32,
    /// One or two plain sentences.
    pub sentence: String,
    /// The same without the caveat, for lists that state it once.
    pub short: String,
}

/// One instrument.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardRow {
    /// What it is.
    #[serde(flatten)]
    pub meta: BoardMeta,
    /// Session of the last close.
    pub session: Option<NaiveDate>,
    /// Last close.
    pub close: Option<f64>,
    /// Simple return over the last 21 sessions.
    pub change_1m: Option<f64>,
    /// Price signals with a value.
    pub readings: Vec<Reading>,
    /// Validated combination by horizon (only when the instrument was scored).
    pub combined: Vec<Combined>,
    /// Blended outlook by horizon (only where the outlook could be computed).
    pub outlook: Vec<OutlookSummary>,
    /// A setup that fired on the latest session.
    pub setup: Option<SetupView>,
    /// The read.
    pub read: Read,
}

/// How the outlook has done, at one horizon, over the holdings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlookRecord {
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Replays scored.
    pub replays: u32,
    /// Share of replays where the median got the direction right.
    pub direction_hit_rate: f64,
    /// Share inside the 90% band.
    pub inside_90: f64,
    /// CRPS skill against "no change".
    pub skill_vs_no_change: f64,
}

/// `GET /signals/board`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalBoard {
    /// Latest session across rows.
    pub session: Option<NaiveDate>,
    /// Instruments held / on the watchlist.
    pub held: u32,
    /// Instruments from the watchlist.
    pub watched: u32,
    /// Signals that passed validation, e.g. "momentum_12_1 h60 t=2.4".
    pub passed: Vec<String>,
    /// Combined IC per signal horizon (0 when nothing passed).
    pub combined_ic: Vec<(u32, f64)>,
    /// Every signal's skill, for the detail view.
    pub skill: Vec<crate::signals::SignalSkill>,
    /// The outlook's record by horizon.
    pub outlook_record: Vec<OutlookRecord>,
    /// When the trade setups last ran.
    pub setups_session: Option<NaiveDate>,
    /// Holdings first (by symbol), then the watchlist (by symbol).
    pub rows: Vec<BoardRow>,
    /// Plain notes on what the board can and can't say.
    pub notes: Vec<String>,
}

/// One instrument's inputs.
pub struct RowInput {
    /// What it is.
    pub meta: BoardMeta,
    /// Closes, oldest first.
    pub closes: Vec<(NaiveDate, f64)>,
    /// Raw price signal values at the latest close.
    pub raw: Vec<(SignalId, f64)>,
    /// Outlook summaries.
    pub outlook: Vec<OutlookSummary>,
    /// A setup that fired.
    pub setup: Option<SetupView>,
}

/// The plain description of a raw reading.
pub fn reading_text(signal: SignalId, raw: f64) -> String {
    let pct = |x: f64| format!("{:.0}%", x.abs() * 100.0);
    let ret = |log: f64| log.exp() - 1.0;
    match signal {
        SignalId::Momentum12_1 => {
            let r = ret(raw);
            format!(
                "{} {} over the past year, skipping the last month",
                if r >= 0.0 { "Up" } else { "Down" },
                pct(r)
            )
        }
        SignalId::ShortTermReversal => {
            // The raw value is minus last month's log return.
            let r = ret(-raw);
            format!(
                "{} {} last month (a fall scores well: short-term moves tend to reverse)",
                if r >= 0.0 { "Up" } else { "Down" },
                pct(r)
            )
        }
        SignalId::LowVolatility => format!(
            "Swings about {:.0}% a year (calmer scores well)",
            -raw * 252f64.sqrt() * 100.0
        ),
        SignalId::Trend => {
            let r = ret(raw);
            if r.abs() < 0.005 {
                return "At its 200-day average".into();
            }
            format!(
                "{} {} its 200-day average",
                pct(r),
                if r >= 0.0 { "above" } else { "below" }
            )
        }
        SignalId::FoundationForecast => {
            let r = ret(raw);
            format!(
                "Chronos's median forecast: {} {} over the next 3 months",
                if r >= 0.0 { "up" } else { "down" },
                pct(r)
            )
        }
        SignalId::ChartPattern | SignalId::ChartTrend => {
            let what = if signal == SignalId::ChartTrend {
                "trend"
            } else {
                "patterns"
            };
            if raw.abs() < 1e-9 {
                format!("No directional chart {what} at the latest bars")
            } else {
                format!(
                    "{} chart {what} at the latest bars (score {raw:+.2})",
                    if raw > 0.0 { "Rising" } else { "Falling" }
                )
            }
        }
        _ => format!("Score {raw:+.3}"),
    }
}

/// Short name of a signal for sentences.
pub fn signal_word(signal: SignalId) -> &'static str {
    match signal {
        SignalId::Momentum12_1 => "momentum",
        SignalId::ShortTermReversal => "reversal",
        SignalId::LowVolatility => "low volatility",
        SignalId::Trend => "trend",
        SignalId::ChartPattern => "chart patterns",
        SignalId::ChartTrend => "chart trend",
        SignalId::FoundationForecast => "foundation model",
        SignalId::FilingSentiment => "filing sentiment",
        SignalId::InsiderBuying => "insider buying",
    }
}

/// Percentile rank (0–100) of `x` among `values`, with ties at their midpoint.
fn percentile(values: &[f64], x: f64) -> f64 {
    if values.len() <= 1 {
        return 50.0;
    }
    let below = values.iter().filter(|v| **v < x).count() as f64;
    let equal = values.iter().filter(|v| **v == x).count() as f64;
    let rank = below + (equal - 1.0) / 2.0;
    (rank / (values.len() as f64 - 1.0) * 100.0).clamp(0.0, 100.0)
}

/// Chance of ending above the start, from the horizon quantiles of the log return at
/// `outlook::LEVELS`. Linear between levels, clamped to 2–98% outside the band.
pub fn p_up(quantiles: &[f64; 5]) -> f64 {
    let levels = crate::outlook::LEVELS;
    if 0.0 <= quantiles[0] {
        return 0.98;
    }
    if quantiles[4] <= 0.0 {
        return 0.02;
    }
    for i in 0..4 {
        let (a, b) = (quantiles[i], quantiles[i + 1]);
        if a <= 0.0 && 0.0 <= b {
            let below = if b > a {
                levels[i] + (0.0 - a) / (b - a) * (levels[i + 1] - levels[i])
            } else {
                (levels[i] + levels[i + 1]) / 2.0
            };
            return 1.0 - below;
        }
    }
    0.5
}

/// The outlook summary from an outlook's last fan point.
pub fn summarize_outlook(o: &crate::outlook::Outlook) -> Option<OutlookSummary> {
    if !matches!(o.status, crate::forecast::ForecastStatus::Ok) || o.origin_close <= 0.0 {
        return None;
    }
    let last = o.fan.last()?;
    let r = |p: f64| p / o.origin_close - 1.0;
    let logs = last.prices.map(|p| (p / o.origin_close).ln());
    if logs.iter().any(|x| !x.is_finite()) {
        return None;
    }
    Some(OutlookSummary {
        horizon_sessions: o.horizon,
        median: r(last.prices[2]),
        low: r(last.prices[0]),
        high: r(last.prices[4]),
        p_up: p_up(&logs),
    })
}

fn combined_for(report: &SignalReport, key: &str) -> Vec<Combined> {
    let mut out: Vec<Combined> = report
        .latest
        .iter()
        .filter(|c: &&CombinedScore| c.instrument_key == key)
        .map(|c| Combined {
            horizon_sessions: c.horizon_sessions,
            score: c.combined,
            alpha: c.alpha,
        })
        .collect();
    out.sort_by_key(|c| c.horizon_sessions);
    out
}

fn passed(report: &SignalReport) -> Vec<String> {
    report
        .skill
        .iter()
        .filter(|s| s.ic_used != 0.0)
        .map(|s| {
            format!(
                "{} {} t={:.1}",
                signal_word(s.signal),
                horizon_word(s.horizon_sessions),
                s.t_stat
            )
        })
        .collect()
}

/// "1M", "3M", or "<n>d".
pub fn horizon_word(sessions: u32) -> String {
    match sessions {
        20 | 21 => "1M".into(),
        60 | 63 => "3M".into(),
        126 => "6M".into(),
        252 => "1Y".into(),
        n => format!("{n}d"),
    }
}

fn read_for(row: &BoardRow, report: &SignalReport) -> Read {
    let validated = report.combined_ic.iter().any(|(_, ic)| *ic > 0.0);
    let for_signals: Vec<&Reading> = row
        .readings
        .iter()
        .filter(|r| r.percentile >= AGREE_HIGH)
        .collect();
    let against: Vec<&Reading> = row
        .readings
        .iter()
        .filter(|r| r.percentile <= AGREE_LOW)
        .collect();
    let names = |rs: &[&Reading]| -> String {
        rs.iter()
            .map(|r| signal_word(r.signal))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let for_count = for_signals.len() as u32;
    let against_count = against.len() as u32;

    if row.readings.is_empty() && row.combined.is_empty() {
        return Read {
            lean: Lean::NoData,
            proven: false,
            for_count,
            against_count,
            sentence: "Not enough price history for any signal yet (most need a year).".into(),
            short: "Not enough price history yet".into(),
        };
    }

    // The longest validated horizon with a non-zero alpha wins.
    let best = row
        .combined
        .iter()
        .filter(|c| c.alpha != 0.0)
        .max_by_key(|c| c.horizon_sessions);
    if validated && let Some(c) = best {
        let lean = if c.score >= LEAN_SCORE {
            Lean::Favoured
        } else if c.score <= -LEAN_SCORE {
            Lean::Disfavoured
        } else {
            Lean::Neutral
        };
        let pct = (c.alpha.exp() - 1.0) * 100.0;
        let ic = report
            .combined_ic
            .iter()
            .find(|(h, _)| *h == c.horizon_sessions)
            .map(|(_, ic)| *ic)
            .unwrap_or(0.0);
        let short = format!(
            "Validated signals expect {}{:.1}% against the board over {}",
            if pct >= 0.0 { "+" } else { "−" },
            pct.abs(),
            if c.horizon_sessions >= 60 {
                "3 months"
            } else {
                "1 month"
            }
        );
        return Read {
            lean,
            proven: true,
            for_count,
            against_count,
            short,
            sentence: format!(
                "Validated signals expect about {}{:.1}% against the rest of the board over {}. Their combined IC is {ic:.2}: {}",
                if pct >= 0.0 { "+" } else { "−" },
                pct.abs(),
                if c.horizon_sessions >= 60 {
                    "3 months"
                } else {
                    "1 month"
                },
                if ic >= 0.10 {
                    "they have ranked returns clearly better than chance, yet any single stock can still go the other way."
                } else {
                    "they rank returns only a little better than chance."
                }
            ),
        };
    }

    let net = for_count as i32 - against_count as i32;
    let lean = if net >= 2 {
        Lean::Favoured
    } else if net <= -2 {
        Lean::Disfavoured
    } else {
        Lean::Mixed
    };
    let n = row.readings.len();
    let verb = |k: u32| if k == 1 { "is" } else { "are" };
    let lead = match (for_count, against_count) {
        (0, 0) => "No signal stands out against the rest of the board".to_string(),
        (f, 0) => format!("{f} of {n} signals favour it ({})", names(&for_signals)),
        (0, a) => format!(
            "{a} of {n} signals {} against it ({})",
            verb(a),
            names(&against)
        ),
        (f, a) => format!(
            "{f} of {n} signals favour it ({}) and {a} {} against it ({})",
            names(&for_signals),
            verb(a),
            names(&against)
        ),
    };
    Read {
        lean,
        proven: false,
        for_count,
        against_count,
        sentence: format!(
            "{lead}. None has passed out-of-sample checks yet, so this describes the past rather than predicting."
        ),
        short: lead,
    }
}

/// Builds the board.
pub fn build(
    inputs: Vec<RowInput>,
    report: &SignalReport,
    outlook_record: Vec<OutlookRecord>,
    setups_session: Option<NaiveDate>,
) -> SignalBoard {
    // Per-signal values across the board, oriented so higher is more favourable (the
    // raw values already are).
    let mut by_signal: std::collections::BTreeMap<SignalId, Vec<f64>> = Default::default();
    for input in &inputs {
        for (signal, value) in &input.raw {
            if value.is_finite() {
                by_signal.entry(*signal).or_default().push(*value);
            }
        }
    }

    let mut rows: Vec<BoardRow> = inputs
        .into_iter()
        .map(|input| {
            let last = input.closes.last().copied();
            let change_1m = {
                let n = input.closes.len();
                (n > 21 && input.closes[n - 22].1 > 0.0)
                    .then(|| input.closes[n - 1].1 / input.closes[n - 22].1 - 1.0)
            };
            let readings = input
                .raw
                .iter()
                .filter(|(_, v)| v.is_finite())
                .map(|(signal, raw)| Reading {
                    signal: *signal,
                    raw: *raw,
                    text: reading_text(*signal, *raw),
                    percentile: percentile(
                        by_signal.get(signal).map(Vec::as_slice).unwrap_or(&[]),
                        *raw,
                    ),
                })
                .collect();
            let mut row = BoardRow {
                combined: combined_for(report, &input.meta.key),
                meta: input.meta,
                session: last.map(|(d, _)| d),
                close: last.map(|(_, c)| c),
                change_1m,
                readings,
                outlook: input.outlook,
                setup: input.setup,
                read: Read {
                    lean: Lean::NoData,
                    proven: false,
                    for_count: 0,
                    against_count: 0,
                    sentence: String::new(),
                    short: String::new(),
                },
            };
            row.read = read_for(&row, report);
            row
        })
        .collect();
    rows.sort_by(|a, b| {
        b.meta
            .held
            .cmp(&a.meta.held)
            .then_with(|| a.meta.symbol.cmp(&b.meta.symbol))
    });

    let held = rows.iter().filter(|r| r.meta.held).count() as u32;
    let watched = rows.len() as u32 - held;
    let passed = passed(report);
    let mut notes = Vec::new();
    if passed.is_empty() {
        notes.push(format!(
            "No signal has passed out-of-sample checks on these {} instruments yet, so the reads are descriptions of the past, not predictions. Signals need about half a year of weekly cross-sections and a reliably positive record before they count.",
            rows.len()
        ));
    } else {
        notes.push(format!(
            "Validated: {}. Reads lean on these; even so, expect them to be wrong often.",
            passed.join(", ")
        ));
    }
    notes.push(
        "Percentiles rank each instrument against the rest of this board, where 100 is the most favourable. The outlook's median and range come from the blended price model; its record over your holdings is shown above the table.".into(),
    );
    notes.push(
        "Survivorship: the watchlist holds today's large, liquid names, which flatters any signal that favours past winners.".into(),
    );

    SignalBoard {
        session: rows.iter().filter_map(|r| r.session).max(),
        held,
        watched,
        passed,
        combined_ic: report.combined_ic.clone(),
        skill: report.skill.clone(),
        outlook_record,
        setups_session,
        rows,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::{SignalEvalConfig, SignalSkill};

    fn day(n: i64) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Duration::days(n)
    }

    fn meta(symbol: &str, held: bool) -> BoardMeta {
        BoardMeta {
            key: format!("k:{symbol}"),
            symbol: symbol.into(),
            name: symbol.into(),
            class: "stock".into(),
            held,
        }
    }

    fn empty_report() -> SignalReport {
        SignalReport {
            config: SignalEvalConfig::default(),
            skill: Vec::new(),
            combined_ic: vec![(20, 0.0), (60, 0.0)],
            latest: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn input(symbol: &str, held: bool, strength: f64) -> RowInput {
        RowInput {
            meta: meta(symbol, held),
            closes: (0..30).map(|i| (day(i), 100.0 + i as f64)).collect(),
            raw: SignalId::PRICE.iter().map(|s| (*s, strength)).collect(),
            outlook: Vec::new(),
            setup: None,
        }
    }

    #[test]
    fn unvalidated_reads_count_agreement_and_say_so() {
        let board = build(
            vec![
                input("A", true, 3.0),
                input("B", false, 2.0),
                input("C", false, 1.0),
                input("D", true, 0.0),
            ],
            &empty_report(),
            Vec::new(),
            None,
        );
        assert_eq!(board.held, 2);
        assert_eq!(board.rows[0].meta.symbol, "A");
        assert_eq!(board.rows[1].meta.symbol, "D");
        let a = &board.rows[0];
        assert_eq!(a.read.lean, Lean::Favoured);
        assert!(!a.read.proven);
        assert_eq!(a.read.for_count, 4);
        assert!(a.read.sentence.contains("describes the past"));
        let d = &board.rows[1];
        assert_eq!(d.read.lean, Lean::Disfavoured);
        assert!(d.read.sentence.starts_with("4 of 4 signals are against it"));
        assert!(board.passed.is_empty());
        assert!(board.notes[0].contains("No signal has passed"));
        let b = board.rows.iter().find(|r| r.meta.symbol == "B").unwrap();
        assert_eq!(b.read.lean, Lean::Mixed);
        assert!((b.readings[0].percentile - 66.666).abs() < 0.01);
        assert!(b.change_1m.is_some());
    }

    #[test]
    fn a_validated_combination_decides_the_read() {
        let mut report = empty_report();
        report.combined_ic = vec![(20, 0.0), (60, 0.08)];
        report.skill.push(SignalSkill {
            signal: SignalId::Momentum12_1,
            horizon_sessions: 60,
            origins: 100,
            ic_mean: 0.1,
            ic_se: 0.02,
            t_stat: 5.0,
            ic_used: 0.06,
            weight: 1.0,
        });
        report.latest.push(CombinedScore {
            instrument_key: "k:A".into(),
            symbol: "A".into(),
            horizon_sessions: 60,
            z: Vec::new(),
            combined: -1.2,
            alpha: -0.02,
            sigma: 0.1,
        });
        // A's readings are all strong, but the validated combination says otherwise.
        let board = build(
            vec![input("A", true, 3.0), input("B", false, 0.0)],
            &report,
            Vec::new(),
            None,
        );
        let a = &board.rows[0];
        assert_eq!(a.read.lean, Lean::Disfavoured);
        assert!(a.read.proven);
        assert!(a.read.sentence.contains("−2.0%"), "{}", a.read.sentence);
        assert_eq!(board.passed, vec!["momentum 3M t=5.0".to_string()]);
    }

    #[test]
    fn no_history_is_no_data() {
        let mut i = input("A", true, 1.0);
        i.raw.clear();
        let board = build(vec![i], &empty_report(), Vec::new(), None);
        assert_eq!(board.rows[0].read.lean, Lean::NoData);
    }

    #[test]
    fn chance_up_reads_the_quantiles() {
        assert!((p_up(&[-0.2, -0.1, 0.0, 0.1, 0.2]) - 0.5).abs() < 1e-12);
        assert_eq!(p_up(&[0.01, 0.02, 0.03, 0.04, 0.05]), 0.98);
        assert_eq!(p_up(&[-0.5, -0.4, -0.3, -0.2, -0.1]), 0.02);
        // Zero halfway between the 16% and 50% quantiles: 33% below, 67% up.
        assert!((p_up(&[-0.3, -0.1, 0.1, 0.2, 0.3]) - 0.67).abs() < 1e-9);
    }

    #[test]
    fn reading_texts_are_plain() {
        assert_eq!(
            reading_text(SignalId::Momentum12_1, 0.3f64.ln_1p()),
            "Up 30% over the past year, skipping the last month"
        );
        assert_eq!(
            reading_text(SignalId::Trend, (-0.1f64).ln_1p()),
            "10% below its 200-day average"
        );
        assert!(reading_text(SignalId::ShortTermReversal, 0.1).starts_with("Down 10% last month"));
        assert_eq!(
            reading_text(SignalId::FoundationForecast, 0.05f64.ln_1p()),
            "Chronos's median forecast: up 5% over the next 3 months"
        );
        assert_eq!(
            reading_text(SignalId::ChartTrend, -0.4),
            "Falling chart trend at the latest bars (score -0.40)"
        );
        assert_eq!(
            reading_text(SignalId::ChartPattern, 0.0),
            "No directional chart patterns at the latest bars"
        );
    }

    #[test]
    fn model_readings_rank_with_the_rest() {
        let mut a = input("A", true, 1.0);
        let mut b = input("B", false, 1.0);
        a.raw.push((SignalId::FoundationForecast, 0.04));
        b.raw.push((SignalId::FoundationForecast, -0.02));
        let board = build(vec![a, b], &empty_report(), Vec::new(), None);
        let fa = board.rows[0]
            .readings
            .iter()
            .find(|r| r.signal == SignalId::FoundationForecast)
            .unwrap();
        assert_eq!(fa.percentile, 100.0);
        assert_eq!(board.rows[0].readings.len(), 5);
    }
}
