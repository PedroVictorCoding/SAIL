//! Dependency-free SVG chart. Only finite validated numbers and UTC ISO dates enter it.
use super::{ForecastStep, Observation};
use std::fmt::Write;
pub(super) fn svg(history: &[Observation], forecast: &[ForecastStep]) -> String {
    let low = history
        .iter()
        .map(|o| o.close)
        .chain(forecast.iter().map(|p| p.deciles[0]))
        .fold(f64::INFINITY, f64::min);
    let high = history
        .iter()
        .map(|o| o.close)
        .chain(forecast.iter().map(|p| p.deciles[8]))
        .fold(f64::NEG_INFINITY, f64::max);
    let pad = ((high - low) * 0.08).max(high.abs() * 0.001).max(0.001);
    let low = low - pad;
    let high = high + pad;
    let y = |v: f64| 260. - (v - low) / (high - low) * 230.;
    let hx = |i: usize| {
        if history.len() == 1 {
            530.
        } else {
            70. + i as f64 / history.len().saturating_sub(1).max(1) as f64 * 460.
        }
    };
    let fx = |step: usize| 530. + step as f64 / forecast.len().max(1) as f64 * 200.;
    let mut s = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 780 330" role="img" aria-labelledby="forecast-title forecast-desc"><title id="forecast-title">Observed closes and TimesFM median forecast</title><desc id="forecast-desc">Solid blue: observed closes. Dashed gold: forecast median. Shaded gold: uncalibrated model tenth to ninetieth percentiles. History and forecast use separate horizontal scales. Exact values are in the forecast table.</desc>"#,
    );
    for i in 0..=4 {
        let v = low + (high - low) * i as f64 / 4.;
        let yy = y(v);
        let _ = write!(
            s,
            r##"<path d="M70 {yy:.2}H730" stroke="#354051"/><text x="62" y="{:.2}" text-anchor="end" fill="#bec8d6" font-size="12">{v:.2}</text>"##,
            yy + 4.
        );
    }
    let mut observed = String::new();
    // Bucket extrema preserve spikes without shipping 15,000 path vertices.
    let bucket = history.len().div_ceil(600).max(1);
    for (index, chunk) in history.chunks(bucket).enumerate() {
        let min = chunk
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.close.total_cmp(&b.close))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let max = chunk
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.close.total_cmp(&b.close))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut indices = vec![0, min, max, chunk.len() - 1];
        indices.sort_unstable();
        indices.dedup();
        for offset in indices {
            let i = index * bucket + offset;
            let _ = write!(
                observed,
                "{} {:.2} {:.2}",
                if observed.is_empty() { "M" } else { "L" },
                hx(i),
                y(history[i].close)
            );
        }
    }
    let reference = history.last().map(|o| o.close).unwrap_or(0.);
    let mut median = format!("M530 {:.2}", y(reference));
    let mut band = format!("M530 {:.2}", y(reference));
    for p in forecast {
        let _ = write!(median, "L{:.2} {:.2}", fx(p.step), y(p.median));
        let _ = write!(band, "L{:.2} {:.2}", fx(p.step), y(p.deciles[0]));
    }
    for p in forecast.iter().rev() {
        let _ = write!(band, "L{:.2} {:.2}", fx(p.step), y(p.deciles[8]));
    }
    band.push('Z');
    let first = history
        .first()
        .map(|b| b.time.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let last = history
        .last()
        .map(|b| b.time.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let _ = write!(
        s,
        r##"<path d="{band}" fill="#e5be79" fill-opacity=".15"/><path d="{observed}" fill="none" stroke="#82b7fa" stroke-width="2"/><path d="{median}" fill="none" stroke="#e5be79" stroke-width="2" stroke-dasharray="6 4"/><path d="M530 25V265" stroke="#aeb8c8" stroke-dasharray="3 5"/><g fill="#bec8d6" font-size="12"><text x="70" y="283">{first}</text><text x="530" y="283" text-anchor="end">{last}</text><text x="730" y="283" text-anchor="end">+{} steps</text><text x="70" y="310">History · observation order</text><text x="545" y="310">Forecast · separate step scale</text></g></svg>"##,
        forecast.len()
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    #[test]
    fn constant_single_bar_and_long_paths_remain_finite_and_bounded() {
        let time = Utc::now();
        let history = vec![Observation { time, close: 100. }; 15_000];
        let path = vec![ForecastStep {
            step: 1,
            calendar_time: time,
            median: 100.,
            deciles: [100.; 9],
        }];
        for bars in [&history[..1], &history[..]] {
            let chart = svg(bars, &path);
            assert!(!chart.contains("NaN"));
            assert!(!chart.contains("inf"));
            assert!(chart.contains("separate step scale"));
            assert!(chart.len() < 100_000);
            assert!(chart.contains("stroke-dasharray"));
        }
    }
}
