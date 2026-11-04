//! Prediction-market probabilities from Polymarket's public Gamma API.
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Outcome {
    pub name: String,
    pub probability: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Market {
    pub slug: String,
    pub question: String,
    pub outcomes: Vec<Outcome>,
    /// Change in the first outcome's probability over 24 hours, in percentage points.
    pub change_24h_pts: Option<f64>,
    pub volume_24h: Option<f64>,
    pub ends: Option<String>,
    pub url: String,
}

const BASE: &str = "https://gamma-api.polymarket.com/markets";

pub fn markets_url(slugs: &[String]) -> String {
    let mut u = url::Url::parse(BASE).expect("static url");
    {
        let mut q = u.query_pairs_mut();
        if slugs.is_empty() {
            q.append_pair("limit", "60")
                .append_pair("active", "true")
                .append_pair("closed", "false")
                .append_pair("order", "volume24hr")
                .append_pair("ascending", "false");
        } else {
            for s in slugs {
                q.append_pair("slug", s);
            }
        }
    }
    u.to_string()
}

/// Gamma encodes some arrays as JSON strings.
fn string_array(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => serde_json::from_str::<Vec<Value>>(s)
            .unwrap_or_default()
            .into_iter()
            .map(|x| match x {
                Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| x.to_string())
            })
            .collect(),
        _ => vec![],
    }
}

/// Parses markets. With `auto` (no slugs chosen), only contested markets are kept: those
/// resolving within two days (the day's sports props that dominate by volume) and those
/// with an outcome already above 95% say little, so they are skipped.
pub fn parse_markets(body: &Value, auto: bool, now: i64, max: usize) -> Vec<Market> {
    body.as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let slug = m["slug"].as_str()?.to_string();
            let question: String = m["question"].as_str()?.chars().take(200).collect();
            let names = string_array(&m["outcomes"]);
            let prices = string_array(&m["outcomePrices"]);
            let outcomes: Vec<Outcome> = names
                .into_iter()
                .zip(prices)
                .filter_map(|(name, p)| {
                    Some(Outcome {
                        name: name.chars().take(60).collect(),
                        probability: p.parse::<f64>().ok().filter(|p| (0.0..=1.0).contains(p))?,
                    })
                })
                .collect();
            if outcomes.is_empty() {
                return None;
            }
            let ends = m["endDate"].as_str().map(str::to_string);
            if auto {
                let end_ts = ends
                    .as_deref()
                    .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
                    .map(|d| d.timestamp());
                if end_ts.is_none_or(|e| e < now + 2 * 86_400)
                    || outcomes.iter().any(|o| o.probability > 0.95)
                {
                    return None;
                }
            }
            Some(Market {
                url: format!("https://polymarket.com/market/{slug}"),
                slug,
                question,
                outcomes,
                change_24h_pts: m["oneDayPriceChange"].as_f64().map(|c| c * 100.0),
                volume_24h: m["volume24hr"].as_f64(),
                ends,
            })
        })
        .take(max)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_urls() {
        assert!(markets_url(&[]).contains("order=volume24hr"));
        assert_eq!(
            markets_url(&["a-b".into(), "c".into()]),
            "https://gamma-api.polymarket.com/markets?slug=a-b&slug=c"
        );
    }

    #[test]
    fn parses_and_filters_short_markets() {
        let now = 1_793_800_000;
        let body = json!([
            {"slug":"soon","question":"Today's game?","outcomes":"[\"Yes\",\"No\"]","outcomePrices":"[\"0.5\",\"0.5\"]","endDate":"2026-11-04T20:00:00Z"},
            {"slug":"later","question":"Will X happen by 2027?","outcomes":"[\"Yes\",\"No\"]","outcomePrices":"[\"0.62\",\"0.38\"]",
             "oneDayPriceChange":0.05,"volume24hr":1000.0,"endDate":"2027-06-30T00:00:00Z"},
            {"slug":"broken","question":"?","outcomes":"[\"Yes\"]","outcomePrices":"[\"nope\"]","endDate":"2027-06-30T00:00:00Z"},
            {"slug":"settled","question":"Near certain?","outcomes":"[\"Yes\",\"No\"]","outcomePrices":"[\"0.99\",\"0.01\"]","endDate":"2027-06-30T00:00:00Z"}
        ]);
        let auto = parse_markets(&body, true, now, 10);
        assert_eq!(auto.len(), 1);
        assert_eq!(auto[0].slug, "later");
        assert_eq!(auto[0].outcomes[0].probability, 0.62);
        assert!((auto[0].change_24h_pts.unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(parse_markets(&body, false, now, 10).len(), 3);
    }
}
