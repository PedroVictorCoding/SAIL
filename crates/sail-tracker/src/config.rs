//! The owner's watchlists and board layout. Everything the tracker fetches is derived
//! from this one document, which is stored locally and validated before it is saved.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_PLACES: usize = 12;
pub const MAX_TICKERS: usize = 30;
pub const MAX_TOPICS: usize = 12;
pub const MAX_FEEDS: usize = 30;
pub const MAX_ROUTES: usize = 10;
pub const MAX_MARKETS: usize = 20;
pub const MAX_CURRENCIES: usize = 12;
pub const MAX_COUNTDOWNS: usize = 20;
pub const MAX_PINNED: usize = 16;
/// OpenSky's anonymous allowance is small; only the first few places get air traffic.
pub const MAX_AIR_PLACES: usize = 4;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Place {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ticker {
    pub symbol: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub id: String,
    pub name: String,
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Countdown {
    pub label: String,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Module {
    Anomalies,
    Numbers,
    Tape,
    Markets,
    News,
    Odds,
    Conditions,
    Routes,
    Clock,
    Map,
    Hazards,
}

impl Module {
    pub const ALL: [Module; 11] = [
        Module::Anomalies,
        Module::Tape,
        Module::Numbers,
        Module::Clock,
        Module::Map,
        Module::News,
        Module::Markets,
        Module::Odds,
        Module::Conditions,
        Module::Routes,
        Module::Hazards,
    ];
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub module: Module,
    /// Width in columns of a 12-column grid.
    pub span: u8,
    /// Height in 60-pixel grid rows.
    pub rows: u8,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct TrackerConfig {
    pub places: Vec<Place>,
    pub tickers: Vec<Ticker>,
    /// Free-text news searches.
    pub topics: Vec<String>,
    /// RSS or Atom feeds.
    pub feeds: Vec<Feed>,
    pub routes: Vec<Route>,
    /// Polymarket market slugs. Empty means the busiest open markets.
    pub markets: Vec<String>,
    pub base_currency: String,
    pub currencies: Vec<String>,
    pub countdowns: Vec<Countdown>,
    /// Metric keys shown in the Numbers panel, such as `quote:^GSPC` or `fx:EUR`.
    pub pinned: Vec<String>,
    pub layout: Vec<Panel>,
}

fn place(id: &str, name: &str, lat: f64, lon: f64) -> Place {
    Place {
        id: id.into(),
        name: name.into(),
        lat,
        lon,
    }
}

fn ticker(symbol: &str, label: &str) -> Ticker {
    Ticker {
        symbol: symbol.into(),
        label: label.into(),
    }
}

fn feed(name: &str, url: &str) -> Feed {
    Feed {
        name: name.into(),
        url: url.into(),
    }
}

pub fn default_layout() -> Vec<Panel> {
    let p = |module, span, rows| Panel {
        module,
        span,
        rows,
        hidden: false,
    };
    vec![
        p(Module::Anomalies, 12, 2),
        p(Module::Tape, 12, 1),
        p(Module::Numbers, 8, 3),
        p(Module::Clock, 4, 3),
        p(Module::Map, 7, 8),
        p(Module::News, 5, 8),
        p(Module::Markets, 5, 6),
        p(Module::Odds, 4, 6),
        p(Module::Conditions, 3, 6),
        p(Module::Routes, 6, 6),
        p(Module::Hazards, 6, 6),
    ]
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            places: vec![
                place("dublin", "Dublin", 53.3498, -6.2603),
                place("belfast", "Belfast", 54.5973, -5.9301),
                place("london", "London", 51.5072, -0.1276),
                place("new-york", "New York", 40.7128, -74.006),
            ],
            tickers: vec![
                ticker("^GSPC", "S&P 500"),
                ticker("^IXIC", "Nasdaq"),
                ticker("^VIX", "VIX"),
                ticker("^ISEQ", "ISEQ"),
                ticker("CL=F", "Crude oil"),
                ticker("GC=F", "Gold"),
                ticker("BTC-USD", "Bitcoin"),
                ticker("^TNX", "US 10Y"),
            ],
            topics: vec![
                "Ireland".into(),
                "semiconductors".into(),
                "LLM training".into(),
                "AI inference infrastructure".into(),
            ],
            feeds: vec![
                feed("BBC World", "https://feeds.bbci.co.uk/news/world/rss.xml"),
                feed("RTÉ News", "https://www.rte.ie/feeds/rss/?index=/news/"),
                feed("Al Jazeera", "https://www.aljazeera.com/xml/rss/all.xml"),
                feed("NPR", "https://feeds.npr.org/1001/rss.xml"),
                feed("The Guardian", "https://www.theguardian.com/world/rss"),
                feed("Hacker News", "https://hnrss.org/frontpage"),
                feed("UNSONG", "https://unsongbook.com/feed/"),
                feed(
                    "arXiv: LLM Training",
                    "https://export.arxiv.org/api/query?search_query=(cat:cs.CL+OR+cat:cs.LG+OR+cat:cs.AI)+AND+(ti:%22language+model%22+OR+ti:LLM+OR+ti:pretraining+OR+ti:post-training+OR+ti:RLHF+OR+ti:GRPO+OR+ti:scaling)&sortBy=submittedDate&sortOrder=descending&max_results=20",
                ),
                feed(
                    "arXiv: Model Infra",
                    "https://export.arxiv.org/api/query?search_query=(cat:cs.LG+OR+cat:cs.DC+OR+cat:cs.CL)+AND+(ti:inference+OR+ti:quantization+OR+ti:distributed+OR+ti:parallelism+OR+ti:kernel+OR+ti:CUDA+OR+ti:MoE+OR+ti:attention)&sortBy=submittedDate&sortOrder=descending&max_results=20",
                ),
            ],
            routes: vec![Route {
                id: "dublin-belfast".into(),
                name: "M1 Dublin → Belfast".into(),
                from: "dublin".into(),
                to: "belfast".into(),
            }],
            markets: vec![],
            base_currency: "EUR".into(),
            currencies: vec!["USD".into(), "GBP".into(), "JPY".into(), "CHF".into()],
            countdowns: vec![],
            pinned: vec![
                "quote:^GSPC".into(),
                "quote:^VIX".into(),
                "quote:BTC-USD".into(),
                "quote:CL=F".into(),
                "fx:USD".into(),
                "fx:GBP".into(),
            ],
            layout: default_layout(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct ConfigError(pub String);

fn bad<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(message.into()))
}

fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_label(s: &str, max: usize) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().count() <= max && !t.chars().any(char::is_control)
}

pub fn is_symbol(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 20
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "^=.-_".contains(c))
}

fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_currency(s: &str) -> bool {
    s.len() == 3 && s.chars().all(|c| c.is_ascii_uppercase())
}

/// Turns a display name into a place id: `New York` → `new-york`.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    out.chars().take(64).collect()
}

fn unique<'a>(what: &str, items: impl Iterator<Item = &'a str>) -> Result<(), ConfigError> {
    let mut seen = HashSet::new();
    for item in items {
        if !seen.insert(item) {
            return bad(format!("Duplicate {what}: {item}"));
        }
    }
    Ok(())
}

fn limit<T>(what: &str, items: &[T], max: usize) -> Result<(), ConfigError> {
    if items.len() > max {
        return bad(format!("At most {max} {what} are allowed"));
    }
    Ok(())
}

impl TrackerConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        limit("places", &self.places, MAX_PLACES)?;
        limit("tickers", &self.tickers, MAX_TICKERS)?;
        limit("topics", &self.topics, MAX_TOPICS)?;
        limit("feeds", &self.feeds, MAX_FEEDS)?;
        limit("routes", &self.routes, MAX_ROUTES)?;
        limit("markets", &self.markets, MAX_MARKETS)?;
        limit("currencies", &self.currencies, MAX_CURRENCIES)?;
        limit("countdowns", &self.countdowns, MAX_COUNTDOWNS)?;
        limit("pinned numbers", &self.pinned, MAX_PINNED)?;

        for p in &self.places {
            if !is_id(&p.id) {
                return bad(format!("Invalid place id: {}", p.id));
            }
            if !is_label(&p.name, 80) {
                return bad("Place names must be 1–80 characters");
            }
            if !(-90.0..=90.0).contains(&p.lat) || !(-180.0..=180.0).contains(&p.lon) {
                return bad(format!("Coordinates out of range for {}", p.name));
            }
        }
        unique("place", self.places.iter().map(|p| p.id.as_str()))?;

        for t in &self.tickers {
            if !is_symbol(&t.symbol) {
                return bad(format!("Invalid ticker symbol: {}", t.symbol));
            }
            if !t.label.is_empty() && !is_label(&t.label, 40) {
                return bad("Ticker labels must be at most 40 characters");
            }
        }
        unique("ticker", self.tickers.iter().map(|t| t.symbol.as_str()))?;

        for topic in &self.topics {
            if !is_label(topic, 80) {
                return bad("Topics must be 1–80 characters");
            }
        }
        unique("topic", self.topics.iter().map(String::as_str))?;

        for f in &self.feeds {
            if !is_label(&f.name, 60) {
                return bad("Feed names must be 1–60 characters");
            }
            match url::Url::parse(&f.url) {
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
                _ => return bad(format!("Feed URL must be http(s): {}", f.url)),
            }
            if f.url.len() > 500 {
                return bad("Feed URLs must be at most 500 characters");
            }
        }
        unique("feed", self.feeds.iter().map(|f| f.url.as_str()))?;

        let place_ids: HashSet<&str> = self.places.iter().map(|p| p.id.as_str()).collect();
        for r in &self.routes {
            if !is_id(&r.id) || !is_label(&r.name, 80) {
                return bad("Routes need an id and a 1–80 character name");
            }
            if !place_ids.contains(r.from.as_str()) || !place_ids.contains(r.to.as_str()) {
                return bad(format!("Route {} must join two configured places", r.name));
            }
            if r.from == r.to {
                return bad(format!(
                    "Route {} starts and ends at the same place",
                    r.name
                ));
            }
        }
        unique("route", self.routes.iter().map(|r| r.id.as_str()))?;

        for m in &self.markets {
            if !is_slug(m) {
                return bad(format!("Invalid market slug: {m}"));
            }
        }
        unique("market", self.markets.iter().map(String::as_str))?;

        if !is_currency(&self.base_currency) {
            return bad("Base currency must be a 3-letter code such as EUR");
        }
        for c in &self.currencies {
            if !is_currency(c) || *c == self.base_currency {
                return bad(format!("Invalid currency: {c}"));
            }
        }
        unique("currency", self.currencies.iter().map(String::as_str))?;

        for c in &self.countdowns {
            if !is_label(&c.label, 60) {
                return bad("Countdown labels must be 1–60 characters");
            }
        }

        for key in &self.pinned {
            if key.len() > 100 || !key.contains(':') || key.chars().any(char::is_control) {
                return bad(format!("Invalid pinned number: {key}"));
            }
        }
        unique("pinned number", self.pinned.iter().map(String::as_str))?;

        let mut modules = HashSet::new();
        for panel in &self.layout {
            if !modules.insert(panel.module) {
                return bad("Each panel may appear once in the layout");
            }
            if !(3..=12).contains(&panel.span) || !(1..=12).contains(&panel.rows) {
                return bad("Panels span 3–12 columns and 1–12 rows");
            }
        }
        Ok(())
    }

    /// Adds panels introduced after this config was saved, so older layouts keep working.
    pub fn complete_layout(&mut self) {
        let present: HashSet<Module> = self.layout.iter().map(|p| p.module).collect();
        for panel in default_layout() {
            if !present.contains(&panel.module) {
                self.layout.push(panel);
            }
        }
    }

    pub fn place(&self, id: &str) -> Option<&Place> {
        self.places.iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        TrackerConfig::default().validate().unwrap();
    }

    #[test]
    fn unsong_rss_is_in_default_feeds() {
        let config = TrackerConfig::default();
        assert!(config.feeds.iter().any(|feed| {
            feed.name == "UNSONG" && feed.url == "https://unsongbook.com/feed/"
        }));
    }

    #[test]
    fn rejects_route_to_unknown_place() {
        let mut c = TrackerConfig::default();
        c.routes[0].to = "atlantis".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_bad_feed_scheme_and_symbol() {
        let mut c = TrackerConfig::default();
        c.feeds[0].url = "file:///etc/passwd".into();
        assert!(c.validate().is_err());
        let mut c = TrackerConfig::default();
        c.tickers[0].symbol = "A B".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_panels_and_odd_spans() {
        let mut c = TrackerConfig::default();
        c.layout.push(c.layout[0].clone());
        assert!(c.validate().is_err());
        let mut c = TrackerConfig::default();
        c.layout[0].span = 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn completes_missing_panels() {
        let mut c = TrackerConfig {
            layout: vec![],
            ..TrackerConfig::default()
        };
        c.complete_layout();
        assert_eq!(c.layout.len(), Module::ALL.len());
    }

    #[test]
    fn slugify_names() {
        assert_eq!(slugify("New York"), "new-york");
        assert_eq!(slugify("  São Paulo!"), "s-o-paulo");
    }
}
