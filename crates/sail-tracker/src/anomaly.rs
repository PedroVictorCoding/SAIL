//! Deviation from a metric's own baseline. Robust statistics (median and MAD) so one
//! spike does not drag the baseline toward itself.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Baseline {
    pub center: f64,
    pub spread: f64,
    pub n: usize,
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Median and scaled MAD of `values`, or `None` with fewer than `min` finite values.
/// `floor` keeps the spread from collapsing to zero for flat series.
pub fn baseline(values: &[f64], min: usize, floor: f64) -> Option<Baseline> {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.len() < min.max(1) {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let center = median(&v);
    let mut dev: Vec<f64> = v.iter().map(|x| (x - center).abs()).collect();
    dev.sort_by(f64::total_cmp);
    let spread = (median(&dev) * 1.4826).max(floor);
    Some(Baseline {
        center,
        spread,
        n: v.len(),
    })
}

/// Sample standard deviation, for series whose spread is the quantity of interest
/// (for example daily returns).
pub fn stdev(values: &[f64]) -> Option<f64> {
    let v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.len() < 3 {
        return None;
    }
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (v.len() - 1) as f64;
    Some(var.sqrt())
}

pub fn z(value: f64, b: &Baseline) -> f64 {
    (value - b.center) / b.spread
}

/// Samples taken at a similar time of day on earlier days, so daily rhythms (night-time
/// air traffic, morning news) are compared with themselves.
pub fn same_time_of_day(samples: &[(i64, f64)], now: i64, window_secs: i64) -> Vec<f64> {
    samples
        .iter()
        .filter(|(ts, _)| now - ts > 6 * 3600)
        .filter(|(ts, _)| {
            let diff = (now - ts).rem_euclid(86_400);
            diff <= window_secs || diff >= 86_400 - window_secs
        })
        .map(|(_, v)| *v)
        .collect()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Anomaly {
    /// The metric key, matching keys in the Numbers panel where one exists.
    pub key: String,
    /// Which panel to look at.
    pub module: &'static str,
    pub label: String,
    pub detail: String,
    /// Signed deviation in baseline spreads (σ-equivalents). Positive means above normal.
    pub z: f64,
}

/// Anomalies at or beyond this many spreads are reported.
pub const THRESHOLD: f64 = 2.0;

/// Keeps the notable anomalies, strongest first.
pub fn rank(mut all: Vec<Anomaly>, max: usize) -> Vec<Anomaly> {
    all.retain(|a| a.z.is_finite() && a.z.abs() >= THRESHOLD);
    all.sort_by(|a, b| b.z.abs().total_cmp(&a.z.abs()));
    all.truncate(max);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_ignores_one_spike() {
        let b = baseline(&[10.0, 11.0, 9.0, 10.0, 10.5, 9.5, 500.0], 5, 0.1).unwrap();
        assert_eq!(b.center, 10.0);
        assert!(b.spread < 2.0);
        assert!(z(500.0, &b) > 100.0);
        assert!(baseline(&[1.0, 2.0], 5, 0.1).is_none());
    }

    #[test]
    fn flat_series_uses_floor() {
        let b = baseline(&[3.0; 10], 5, 0.5).unwrap();
        assert_eq!(b.spread, 0.5);
    }

    #[test]
    fn same_time_of_day_selects_prior_days() {
        let now = 10 * 86_400 + 12 * 3600;
        let samples = vec![
            (now - 86_400, 1.0),            // yesterday, same time
            (now - 86_400 + 1800, 2.0),     // yesterday, 30 minutes later
            (now - 86_400 + 6 * 3600, 3.0), // yesterday evening
            (now - 600, 4.0),               // ten minutes ago
        ];
        assert_eq!(same_time_of_day(&samples, now, 3600), vec![1.0, 2.0]);
    }

    #[test]
    fn rank_filters_and_sorts() {
        let a = |z: f64| Anomaly {
            key: format!("k{z}"),
            module: "markets",
            label: String::new(),
            detail: String::new(),
            z,
        };
        let ranked = rank(vec![a(1.0), a(-3.0), a(2.5), a(f64::NAN)], 10);
        assert_eq!(
            ranked.iter().map(|x| x.z).collect::<Vec<_>>(),
            vec![-3.0, 2.5]
        );
    }
}
