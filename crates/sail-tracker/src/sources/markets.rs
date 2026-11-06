//! Prices and FX. Quotes come from Yahoo's public chart endpoint; FX from the ECB
//! reference rates via Frankfurter. A day's move is judged against the symbol's own
//! recent daily volatility, so a 2% move in the VIX and in a bond yield read differently.
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Quote {
    pub symbol: String,
    pub name: String,
    pub currency: String,
    pub price: f64,
    pub previous_close: f64,
    pub change: f64,
    pub change_pct: f64,
    /// Today's move in units of the symbol's typical daily move (σ of daily returns).
    pub move_z: Option<f64>,
    pub day_high: Option<f64>,
    pub day_low: Option<f64>,
    /// Intraday closes, downsampled for a sparkline.
    pub spark: Vec<f64>,
    pub market_time: i64,
}

pub fn chart_url(symbol: &str, range: &str, interval: &str) -> String {
    // Symbols are validated to [A-Za-z0-9^=.-_]; escape the two reserved characters.
    let symbol = symbol.replace('^', "%5E").replace('=', "%3D");
    format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{symbol}?range={range}&interval={interval}"
    )
}

fn closes(result: &Value) -> Vec<f64> {
    result["indicators"]["quote"][0]["close"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

pub fn downsample(values: &[f64], max: usize) -> Vec<f64> {
    if values.len() <= max || max < 2 {
        return values.to_vec();
    }
    let step = (values.len() - 1) as f64 / (max - 1) as f64;
    (0..max)
        .map(|i| values[((i as f64 * step).round() as usize).min(values.len() - 1)])
        .collect()
}

/// Parses an intraday chart response into a quote (without `move_z`).
pub fn parse_chart(body: &Value, label: &str) -> Option<Quote> {
    let result = &body["chart"]["result"][0];
    let meta = &result["meta"];
    let symbol = meta["symbol"].as_str()?.to_string();
    let price = meta["regularMarketPrice"].as_f64()?;
    let previous_close = meta["previousClose"]
        .as_f64()
        .or_else(|| meta["chartPreviousClose"].as_f64())?;
    if previous_close == 0.0 {
        return None;
    }
    let name = if label.is_empty() {
        meta["shortName"].as_str().unwrap_or(&symbol).to_string()
    } else {
        label.to_string()
    };
    let change = price - previous_close;
    Some(Quote {
        symbol,
        name,
        currency: meta["currency"].as_str().unwrap_or("").to_string(),
        price,
        previous_close,
        change,
        change_pct: change / previous_close * 100.0,
        move_z: None,
        day_high: meta["regularMarketDayHigh"].as_f64(),
        day_low: meta["regularMarketDayLow"].as_f64(),
        spark: downsample(&closes(result), 64),
        market_time: meta["regularMarketTime"].as_i64().unwrap_or(0),
    })
}

/// σ of daily log returns from a daily chart response, excluding the latest (partial) day.
pub fn daily_volatility(body: &Value) -> Option<f64> {
    let c = closes(&body["chart"]["result"][0]);
    if c.len() < 10 {
        return None;
    }
    let settled = &c[..c.len() - 1];
    let returns: Vec<f64> = settled
        .windows(2)
        .filter(|w| w[0] > 0.0 && w[1] > 0.0)
        .map(|w| (w[1] / w[0]).ln())
        .collect();
    crate::anomaly::stdev(&returns).filter(|s| *s > 0.0)
}

pub fn move_z(q: &Quote, daily_sigma: f64) -> f64 {
    (q.price / q.previous_close).ln() / daily_sigma
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Rate {
    pub currency: String,
    /// Units of `currency` per one unit of the base currency.
    pub rate: f64,
    pub previous: Option<f64>,
    pub change_pct: Option<f64>,
    pub date: String,
    pub spark: Vec<f64>,
}

pub fn fx_url(base: &str, currencies: &[String], start: chrono::NaiveDate) -> String {
    let mut u = url::Url::parse(&format!(
        "https://api.frankfurter.dev/v1/{}..",
        start.format("%Y-%m-%d")
    ))
    .expect("static url");
    u.query_pairs_mut()
        .append_pair("base", base)
        .append_pair("symbols", &currencies.join(","));
    u.to_string()
}

pub fn parse_fx(body: &Value, currencies: &[String]) -> Vec<Rate> {
    let Some(rates) = body["rates"].as_object() else {
        return vec![];
    };
    let mut dates: Vec<&String> = rates.keys().collect();
    dates.sort();
    currencies
        .iter()
        .filter_map(|c| {
            let series: Vec<(String, f64)> = dates
                .iter()
                .filter_map(|d| rates[*d][c].as_f64().map(|v| ((*d).clone(), v)))
                .collect();
            let (date, rate) = series.last()?.clone();
            let previous = series.len().checked_sub(2).map(|i| series[i].1);
            Some(Rate {
                currency: c.clone(),
                rate,
                previous,
                change_pct: previous.map(|p| (rate - p) / p * 100.0),
                date,
                spark: series.iter().map(|(_, v)| *v).collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chart_url_escapes_symbols() {
        assert_eq!(
            chart_url("^GSPC", "1d", "5m"),
            "https://query1.finance.yahoo.com/v8/finance/chart/%5EGSPC?range=1d&interval=5m"
        );
    }

    #[test]
    fn parses_chart_and_scores_move() {
        let body = json!({"chart":{"result":[{"meta":{"symbol":"AAPL","currency":"USD","shortName":"Apple Inc.",
            "regularMarketPrice":105.0,"previousClose":100.0,"regularMarketTime":1},
            "indicators":{"quote":[{"close":[100.0,null,102.0,105.0]}]}}]}});
        let q = parse_chart(&body, "").unwrap();
        assert_eq!(q.name, "Apple Inc.");
        assert_eq!(q.spark, vec![100.0, 102.0, 105.0]);
        assert!((q.change_pct - 5.0).abs() < 1e-9);
        assert!((move_z(&q, 0.01) - 4.879).abs() < 0.01);
    }

    #[test]
    fn volatility_needs_history() {
        let mk = |c: Vec<f64>| json!({"chart":{"result":[{"indicators":{"quote":[{"close":c}]}}]}});
        assert!(daily_volatility(&mk(vec![1.0, 2.0])).is_none());
        let alternating: Vec<f64> = (0..20)
            .map(|i| if i % 2 == 0 { 100.0 } else { 101.0 })
            .collect();
        assert!(daily_volatility(&mk(alternating)).unwrap() > 0.005);
    }

    #[test]
    fn parses_fx_series() {
        let body = json!({"rates":{"2026-11-03":{"USD":1.10},"2026-11-04":{"USD":1.12},"2026-11-02":{"USD":1.0}}});
        let r = &parse_fx(&body, &["USD".into(), "XXX".into()])[0];
        assert_eq!(r.date, "2026-11-04");
        assert_eq!(r.previous, Some(1.10));
        assert_eq!(r.spark, vec![1.0, 1.10, 1.12]);
    }

    #[test]
    fn downsample_keeps_ends() {
        let v: Vec<f64> = (0..100).map(f64::from).collect();
        let d = downsample(&v, 5);
        assert_eq!(d.first(), Some(&0.0));
        assert_eq!(d.last(), Some(&99.0));
        assert_eq!(d.len(), 5);
    }
}
