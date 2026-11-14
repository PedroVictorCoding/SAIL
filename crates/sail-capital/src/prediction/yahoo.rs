use super::{Observation, Request, calendar_time};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) struct Data {
    pub bars: Vec<Observation>,
    pub currency: Option<String>,
    pub timezone: String,
    pub adjustment: String,
    pub note: String,
}
pub(super) async fn fetch(request: &Request) -> Result<Data, String> {
    let now = Utc::now();
    let days = match request.interval.as_str() {
        "1m" => Some(7),
        "1h" => Some(729),
        "2m" | "5m" | "15m" | "30m" | "90m" => Some(59),
        _ => None,
    };
    let start = days
        .map(|d| (now - Duration::days(d)).timestamp())
        .unwrap_or(0);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("SAIL-Capital/0.1 stock research")
        .build()
        .map_err(|_| "Cannot initialize Yahoo client.".to_string())?;
    let mut url = reqwest::Url::parse("https://query1.finance.yahoo.com/v8/finance/chart/")
        .map_err(|_| "Invalid Yahoo URL.")?;
    url.path_segments_mut()
        .map_err(|_| "Invalid Yahoo URL.")?
        .pop_if_empty()
        .push(&request.symbol);
    let mut response = client
        .get(url)
        .query(&[
            ("period1", start.to_string()),
            ("period2", now.timestamp().to_string()),
            ("interval", request.interval.clone()),
            ("events", "div,splits".into()),
            ("includeAdjustedClose", "true".into()),
            ("includePrePost", "false".into()),
        ])
        .send()
        .await
        .map_err(|_| "Yahoo request failed or timed out. No forecast was made.".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Yahoo refused the request (HTTP {}). Try later or check the ticker and interval. No access workaround was attempted.",
            response.status().as_u16()
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Cannot read Yahoo response.")?
    {
        if body.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err("Yahoo response exceeded 8 MiB.".into());
        }
        body.extend_from_slice(&chunk);
    }
    let mut data = parse(&body, &request.interval, now)?;
    data.note.push_str(&match days { Some(d)=>format!(" Yahoo intraday history is requested for at most {d} days; availability may be shorter."),None=>" Yahoo history is requested from the earliest available date.".into() });
    Ok(data)
}
fn parse(bytes: &[u8], interval: &str, now: DateTime<Utc>) -> Result<Data, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "Yahoo returned invalid JSON.")?;
    if !value["chart"]["error"].is_null() {
        return Err("Yahoo reported an unavailable ticker or unsupported interval.".into());
    }
    let item = value
        .pointer("/chart/result/0")
        .ok_or("Yahoo returned no instrument.")?;
    let times = item["timestamp"]
        .as_array()
        .ok_or("Yahoo returned no timestamps.")?;
    let adjusted = item
        .pointer("/indicators/adjclose/0/adjclose")
        .and_then(Value::as_array);
    let closes = adjusted
        .or_else(|| {
            item.pointer("/indicators/quote/0/close")
                .and_then(Value::as_array)
        })
        .ok_or("Yahoo returned no closes.")?;
    if closes.len() != times.len() {
        return Err("Yahoo timestamps and prices have different lengths.".into());
    }
    let mut bars = BTreeMap::new();
    let mut skipped = 0;
    for (t, p) in times.iter().zip(closes) {
        let row = t
            .as_i64()
            .and_then(|s| DateTime::from_timestamp(s, 0))
            .zip(p.as_f64());
        if let Some((time, close)) = row
            && close.is_finite()
            && close > 0.
            && calendar_time(time, interval, 1).is_some_and(|end| end <= now)
        {
            bars.insert(time, Observation { time, close });
            continue;
        }
        skipped += 1;
    }
    if bars.is_empty() {
        return Err("Yahoo returned no completed positive price bars.".into());
    }
    let adjustment = if adjusted.is_some() {
        "Yahoo split and dividend adjusted close"
    } else {
        "Yahoo close; adjusted-close field unavailable (dividend adjustment not guaranteed)"
    };
    Ok(Data {
        bars: bars.into_values().collect(),
        currency: item["meta"]["currency"].as_str().map(String::from),
        timezone: item["meta"]["exchangeTimezoneName"]
            .as_str()
            .unwrap_or("unknown")
            .into(),
        adjustment: adjustment.into(),
        note: format!(
            "{skipped} null, nonpositive or incomplete bars excluded. A bar is conservatively complete after one full calendar interval."
        ),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adjusted_prices_nulls_order_and_incomplete_bars() {
        let value = serde_json::json!({"chart":{"error":null,"result":[{"meta":{"currency":"USD","exchangeTimezoneName":"America/New_York"},"timestamp":[200,100,300,400,500],"indicators":{"adjclose":[{"adjclose":[20.,10.,null,-2.,50.]}],"quote":[{"close":[200.,100.,300.,400.,500.]}]}}]}});
        let data = parse(
            &serde_json::to_vec(&value).unwrap(),
            "1m",
            DateTime::from_timestamp(550, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            data.bars.iter().map(|b| b.close).collect::<Vec<_>>(),
            vec![10., 20.]
        );
        assert!(data.adjustment.contains("dividend adjusted"));
    }
    #[test]
    fn malformed_or_refused_data_is_not_forecastable() {
        assert!(parse(b"{}", "1d", Utc::now()).is_err());
        assert!(
            parse(
                br#"{"chart":{"error":{"code":"Not Found"}}}"#,
                "1d",
                Utc::now()
            )
            .is_err()
        );
    }
}
