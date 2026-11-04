//! The tracker engine: refreshes each source on its own cadence while someone is
//! watching, keeps the latest board in memory, derives anomalies, and broadcasts patches.
use crate::anomaly::{self, Anomaly};
use crate::config::{MAX_AIR_PLACES, TrackerConfig};
use crate::net::{FetchError, Net};
use crate::sources::{markets, news, odds, world};
use crate::store::{Store, StoreError, StoredHeadline};
use chrono::Utc;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore, broadcast};

/// The board is refreshed while a view is open or was used within this window.
const IDLE_AFTER_SECS: i64 = 600;
const FEED_MAX_BYTES: usize = 3 * 1024 * 1024;
const API_MAX_BYTES: usize = 4 * 1024 * 1024;
const TILE_MAX_BYTES: usize = 512 * 1024;
const TILE_MAX_AGE_SECS: u64 = 30 * 86_400;
const STORY_WINDOW_SECS: i64 = 36 * 3600;
const MAX_STORIES: usize = 150;

fn now() -> i64 {
    Utc::now().timestamp()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    News,
    Quotes,
    Fx,
    Odds,
    Weather,
    Air,
    Routes,
    Quakes,
}

impl Source {
    pub const ALL: [Source; 8] = [
        Source::News,
        Source::Quotes,
        Source::Fx,
        Source::Odds,
        Source::Weather,
        Source::Air,
        Source::Routes,
        Source::Quakes,
    ];

    pub fn cadence_secs(self) -> i64 {
        match self {
            Source::News => 300,
            Source::Quotes => 60,
            Source::Fx => 3600,
            Source::Odds => 120,
            Source::Weather => 600,
            // OpenSky's anonymous allowance is about 400 requests a day.
            Source::Air => 900,
            Source::Routes => 6 * 3600,
            Source::Quakes => 300,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::News => "News feeds",
            Source::Quotes => "Yahoo Finance",
            Source::Fx => "ECB rates (Frankfurter)",
            Source::Odds => "Polymarket",
            Source::Weather => "Open-Meteo",
            Source::Air => "OpenSky",
            Source::Routes => "OSRM",
            Source::Quakes => "USGS",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| {
            serde_json::to_value(x)
                .ok()
                .as_ref()
                .and_then(Value::as_str)
                == Some(s)
        })
    }
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct SourceStatus {
    pub label: String,
    pub ok: bool,
    pub fetched_at: Option<i64>,
    pub error: Option<String>,
    pub cadence_secs: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TopicVolume {
    pub topic: String,
    /// Stories on this topic first seen in the last six hours.
    pub stories_6h: usize,
    pub usual: Option<f64>,
    pub z: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct Board {
    pub news: Vec<news::Story>,
    pub topics: Vec<TopicVolume>,
    pub quotes: Vec<markets::Quote>,
    pub fx: Vec<markets::Rate>,
    pub odds: Vec<odds::Market>,
    pub conditions: Vec<world::Conditions>,
    pub air: Vec<world::AirTraffic>,
    pub routes: Vec<world::RouteInfo>,
    pub quakes: Vec<world::Quake>,
    pub anomalies: Vec<Anomaly>,
    pub sources: BTreeMap<Source, SourceStatus>,
}

#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("{0}")]
    Invalid(String),
}

pub struct Tracker {
    store: Store,
    net: Net,
    config: RwLock<TrackerConfig>,
    board: RwLock<Board>,
    updates: broadcast::Sender<Arc<Value>>,
    next_due: Mutex<HashMap<Source, i64>>,
    inflight: Mutex<HashSet<Source>>,
    last_view: AtomicI64,
    wake: Notify,
    always_on: bool,
    /// Daily-return σ per symbol, refreshed once a day.
    volatility: Mutex<HashMap<String, (chrono::NaiveDate, Option<f64>)>>,
    tiles: Semaphore,
}

impl Tracker {
    pub fn open_default() -> Result<Arc<Self>, TrackerError> {
        Self::open(&crate::store::default_dir())
    }

    pub fn open(dir: &std::path::Path) -> Result<Arc<Self>, TrackerError> {
        Self::with_store(Store::open(dir)?)
    }

    pub fn with_store(store: Store) -> Result<Arc<Self>, TrackerError> {
        let mut config = store.load_config()?.unwrap_or_default();
        config.complete_layout();
        if config.validate().is_err() {
            tracing::warn!("Stored tracker config is invalid; using defaults");
            config = TrackerConfig::default();
        }
        let (updates, _) = broadcast::channel(64);
        let board = Board {
            sources: Source::ALL
                .into_iter()
                .map(|s| {
                    (
                        s,
                        SourceStatus {
                            label: s.label().into(),
                            cadence_secs: s.cadence_secs(),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        Ok(Arc::new(Self {
            store,
            net: Net::new(),
            config: RwLock::new(config),
            board: RwLock::new(board),
            updates,
            next_due: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashSet::new()),
            last_view: AtomicI64::new(0),
            wake: Notify::new(),
            always_on: std::env::var("SAIL_TRACKER_ALWAYS_ON").is_ok_and(|v| v == "1"),
            volatility: Mutex::new(HashMap::new()),
            tiles: Semaphore::new(4),
        }))
    }

    pub fn config(&self) -> TrackerConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_config(&self, mut config: TrackerConfig) -> Result<TrackerConfig, TrackerError> {
        config.complete_layout();
        config
            .validate()
            .map_err(|e| TrackerError::Invalid(e.to_string()))?;
        self.store.save_config(&config)?;
        let previous = std::mem::replace(
            &mut *self.config.write().unwrap_or_else(|e| e.into_inner()),
            config.clone(),
        );
        // Refresh only what the change affects; layout edits refetch nothing.
        let mut due = Vec::new();
        if previous.feeds != config.feeds || previous.topics != config.topics {
            due.push(Source::News);
        }
        if previous.tickers != config.tickers {
            due.push(Source::Quotes);
        }
        if previous.base_currency != config.base_currency
            || previous.currencies != config.currencies
        {
            due.push(Source::Fx);
        }
        if previous.markets != config.markets {
            due.push(Source::Odds);
        }
        if previous.places != config.places {
            due.extend([Source::Weather, Source::Air, Source::Routes, Source::Quakes]);
        } else if previous.routes != config.routes {
            due.push(Source::Routes);
        }
        self.mark_due(&due);
        Ok(config)
    }

    pub fn mark_due(&self, sources: &[Source]) {
        {
            let mut next = self.next_due.lock().unwrap_or_else(|e| e.into_inner());
            for s in sources {
                next.insert(*s, 0);
            }
        }
        self.touch();
        self.wake.notify_one();
    }

    /// Records that the board is being looked at, which keeps refreshes running.
    pub fn touch(&self) {
        self.last_view.store(now(), Ordering::Relaxed);
    }

    fn active(&self) -> bool {
        self.always_on
            || self.updates.receiver_count() > 0
            || now() - self.last_view.load(Ordering::Relaxed) < IDLE_AFTER_SECS
    }

    pub fn board(&self) -> Value {
        serde_json::to_value(&*self.board.read().unwrap_or_else(|e| e.into_inner()))
            .unwrap_or(Value::Null)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Value>> {
        self.touch();
        self.updates.subscribe()
    }

    /// Starts the refresh loop. Call once.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move { me.run().await });
    }

    async fn run(self: Arc<Self>) {
        let mut last_prune = 0;
        loop {
            if self.active() {
                let t = now();
                let due: Vec<Source> = {
                    let next = self.next_due.lock().unwrap_or_else(|e| e.into_inner());
                    let inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
                    Source::ALL
                        .into_iter()
                        .filter(|s| next.get(s).copied().unwrap_or(0) <= t && !inflight.contains(s))
                        .collect()
                };
                for source in due {
                    self.inflight
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(source);
                    let me = self.clone();
                    tokio::spawn(async move {
                        me.refresh(source).await;
                        me.inflight
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&source);
                    });
                }
                if t - last_prune > 3600 {
                    last_prune = t;
                    if let Err(e) = self.store.prune(t) {
                        tracing::warn!(error = %e, "Tracker prune failed");
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = self.wake.notified() => {}
            }
        }
    }

    /// Refreshes one source now and broadcasts the result.
    pub async fn refresh(&self, source: Source) {
        let config = self.config();
        let started = now();
        let result = match source {
            Source::News => self.refresh_news(&config).await,
            Source::Quotes => self.refresh_quotes(&config).await,
            Source::Fx => self.refresh_fx(&config).await,
            Source::Odds => self.refresh_odds(&config).await,
            Source::Weather => self.refresh_weather(&config).await,
            Source::Air => self.refresh_air(&config).await,
            Source::Routes => self.refresh_routes(&config).await,
            Source::Quakes => self.refresh_quakes(&config).await,
        };
        let mut wait = source.cadence_secs();
        let error = match result {
            Ok(partial) => partial,
            Err(e) => {
                if matches!(e, TrackerError::Fetch(FetchError::Status(429))) {
                    wait = wait.max(1800);
                }
                tracing::debug!(?source, error = %e, "Tracker source failed");
                Some(e.to_string())
            }
        };
        self.next_due
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(source, started + wait);
        let patch = {
            let mut board = self.board.write().unwrap_or_else(|e| e.into_inner());
            let status = board.sources.entry(source).or_default();
            status.ok = error.is_none();
            status.error = error;
            if status.ok || status.fetched_at.is_none() {
                status.fetched_at = Some(now());
            }
            board.anomalies = derive_anomalies(&board);
            let value = serde_json::to_value(&*board).unwrap_or(Value::Null);
            let mut patch = serde_json::Map::new();
            for key in fields(source).iter().chain(&["anomalies", "sources"]) {
                patch.insert((*key).into(), value[*key].clone());
            }
            Value::Object(patch)
        };
        let _ = self.updates.send(Arc::new(patch));
    }

    fn update(&self, f: impl FnOnce(&mut Board)) {
        f(&mut self.board.write().unwrap_or_else(|e| e.into_inner()));
    }

    // ------------------------------------------------------------ sources

    /// Returns `Ok(Some(message))` when some feeds failed but others worked.
    async fn refresh_news(&self, c: &TrackerConfig) -> Result<Option<String>, TrackerError> {
        let t = now();
        let mut jobs: Vec<(String, Option<String>, String, bool)> = c
            .feeds
            .iter()
            .map(|f| (f.name.clone(), None, f.url.clone(), true))
            .collect();
        for topic in &c.topics {
            let mut u = url::Url::parse("https://news.google.com/rss/search").expect("static");
            u.query_pairs_mut()
                .append_pair("q", &format!("{topic} when:1d"))
                .append_pair("hl", "en")
                .append_pair("gl", "US")
                .append_pair("ceid", "US:en");
            jobs.push((
                "Google News".into(),
                Some(topic.clone()),
                u.to_string(),
                false,
            ));
        }
        let total = jobs.len();
        let results: Vec<_> = futures_util::stream::iter(jobs)
            .map(|(name, topic, url, guarded)| async move {
                let body = if guarded {
                    self.net.get_guarded(&url, FEED_MAX_BYTES).await
                } else {
                    self.net.get_bytes(&url, FEED_MAX_BYTES).await
                };
                let items = body.map_err(|e| e.to_string()).and_then(|b| {
                    news::parse_feed(&String::from_utf8_lossy(&b)).map_err(|e| e.to_string())
                });
                (name, topic, items)
            })
            .buffer_unordered(6)
            .collect()
            .await;

        let topic_lc: Vec<(String, String)> = c
            .topics
            .iter()
            .map(|x| (x.clone(), x.to_lowercase()))
            .collect();
        let mut fresh = Vec::new();
        let mut failures = Vec::new();
        for (name, topic, items) in results {
            let items = match items {
                Ok(items) => items,
                Err(e) => {
                    failures.push(format!("{name}: {e}"));
                    continue;
                }
            };
            for item in items {
                let source = item.source.clone().unwrap_or_else(|| name.clone());
                let title = if item.source.is_some() {
                    news::strip_outlet_suffix(&item.title, &source)
                } else {
                    item.title.clone()
                };
                let published = item.published.unwrap_or(t).min(t);
                if t - published > 3 * 86_400 {
                    continue;
                }
                let topic = topic.clone().or_else(|| {
                    let lower = title.to_lowercase();
                    topic_lc
                        .iter()
                        .find(|(_, lc)| lower.contains(lc.as_str()))
                        .map(|(orig, _)| orig.clone())
                });
                let id = format!(
                    "{}{}",
                    news::headline_id(&title),
                    &news::headline_id(&source)[..6]
                );
                fresh.push(StoredHeadline {
                    id,
                    title,
                    url: item.link,
                    source,
                    topic,
                    published,
                    first_seen: t,
                });
            }
        }
        if failures.len() == total && total > 0 {
            return Err(TrackerError::Invalid(format!(
                "All feeds failed. {}",
                failures.join("; ")
            )));
        }
        self.store.add_headlines(&fresh)?;
        let stored = self.store.headlines(t - STORY_WINDOW_SECS, 600)?;
        let mut stories = news::cluster(stored, 0.6);
        stories.truncate(MAX_STORIES);

        let mut topics = Vec::new();
        for topic in &c.topics {
            let count = stories
                .iter()
                .filter(|s| {
                    s.topic.as_deref() == Some(topic.as_str())
                        && t - s.first_seen.max(s.published) < 6 * 3600
                })
                .count();
            let key = format!("news:{topic}");
            self.store.record(&key, t, count as f64)?;
            let history = self.store.series(&key, t - 14 * 86_400)?;
            let b = anomaly::baseline(&anomaly::same_time_of_day(&history, t, 3600), 5, 1.0);
            topics.push(TopicVolume {
                topic: topic.clone(),
                stories_6h: count,
                usual: b.map(|b| b.center),
                z: b.map(|b| anomaly::z(count as f64, &b)),
            });
        }
        self.update(|b| {
            b.news = stories;
            b.topics = topics;
        });
        Ok((!failures.is_empty()).then(|| {
            format!(
                "{} of {} sources failed: {}",
                failures.len(),
                total,
                failures.join("; ")
            )
        }))
    }

    async fn volatility(&self, symbol: &str) -> Option<f64> {
        let today = Utc::now().date_naive();
        if let Some((date, sigma)) = self
            .volatility
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(symbol)
            && *date == today
        {
            return *sigma;
        }
        let sigma = self
            .net
            .get_json(&markets::chart_url(symbol, "3mo", "1d"), API_MAX_BYTES)
            .await
            .ok()
            .and_then(|b| markets::daily_volatility(&b));
        self.volatility
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(symbol.to_string(), (today, sigma));
        sigma
    }

    async fn refresh_quotes(&self, c: &TrackerConfig) -> Result<Option<String>, TrackerError> {
        let results: Vec<_> = futures_util::stream::iter(c.tickers.iter().cloned().enumerate())
            .map(|(i, t)| async move {
                let body = self
                    .net
                    .get_json(&markets::chart_url(&t.symbol, "1d", "5m"), API_MAX_BYTES)
                    .await;
                let quote = match body {
                    Ok(b) => markets::parse_chart(&b, &t.label)
                        .ok_or_else(|| format!("{}: no quote", t.symbol)),
                    Err(e) => Err(format!("{}: {e}", t.symbol)),
                };
                let quote = match quote {
                    Ok(mut q) => {
                        q.move_z = self
                            .volatility(&t.symbol)
                            .await
                            .map(|s| markets::move_z(&q, s));
                        Ok(q)
                    }
                    Err(e) => Err(e),
                };
                (i, quote)
            })
            .buffer_unordered(6)
            .collect()
            .await;
        let mut results = results;
        results.sort_by_key(|(i, _)| *i);
        let mut quotes = vec![];
        let mut failures = vec![];
        for (_, r) in results {
            match r {
                Ok(q) => quotes.push(q),
                Err(e) => failures.push(e),
            }
        }
        if quotes.is_empty() && !failures.is_empty() {
            return Err(TrackerError::Invalid(failures.join("; ")));
        }
        self.update(|b| b.quotes = quotes);
        Ok((!failures.is_empty()).then(|| failures.join("; ")))
    }

    async fn refresh_fx(&self, c: &TrackerConfig) -> Result<Option<String>, TrackerError> {
        if c.currencies.is_empty() {
            self.update(|b| b.fx.clear());
            return Ok(None);
        }
        let start = Utc::now().date_naive() - chrono::Duration::days(21);
        let body = self
            .net
            .get_json(
                &markets::fx_url(&c.base_currency, &c.currencies, start),
                API_MAX_BYTES,
            )
            .await?;
        let rates = markets::parse_fx(&body, &c.currencies);
        self.update(|b| b.fx = rates);
        Ok(None)
    }

    async fn refresh_odds(&self, c: &TrackerConfig) -> Result<Option<String>, TrackerError> {
        let body = self
            .net
            .get_json(&odds::markets_url(&c.markets), API_MAX_BYTES)
            .await?;
        let mut markets = odds::parse_markets(&body, c.markets.is_empty(), now(), 12);
        if !c.markets.is_empty() {
            // Keep the owner's order.
            markets.sort_by_key(|m| c.markets.iter().position(|s| *s == m.slug));
        }
        self.update(|b| b.odds = markets);
        Ok(None)
    }

    async fn refresh_weather(&self, c: &TrackerConfig) -> Result<Option<String>, TrackerError> {
        if c.places.is_empty() {
            self.update(|b| b.conditions.clear());
            return Ok(None);
        }
        let (weather_url, air_url) = (
            world::weather_url(&c.places),
            world::air_quality_url(&c.places),
        );
        let (weather, air) = tokio::join!(
            self.net.get_json(&weather_url, API_MAX_BYTES),
            self.net.get_json(&air_url, API_MAX_BYTES)
        );
        let weather = weather?;
        let conditions = world::parse_weather(&weather, air.as_ref().ok(), &c.places);
        let t = now();
        for cond in &conditions {
            if let Some(temp) = cond.temp {
                self.store
                    .record(&format!("temp:{}", cond.place), t, temp)?;
            }
        }
        self.update(|b| b.conditions = conditions);
        Ok(air.err().map(|e| format!("Air quality unavailable: {e}")))
    }

    async fn refresh_air(&self, c: &TrackerConfig) -> Result<Option<String>, TrackerError> {
        let t = now();
        let mut out = vec![];
        let mut failures = vec![];
        for place in c.places.iter().take(MAX_AIR_PLACES) {
            match self
                .net
                .get_json(&world::air_url(place), API_MAX_BYTES)
                .await
            {
                Ok(body) => {
                    let mut a = world::parse_air(&body, place);
                    let key = format!("air:{}", place.id);
                    self.store.record(&key, t, a.airborne as f64)?;
                    let history = self.store.series(&key, t - 14 * 86_400)?;
                    if let Some(b) =
                        anomaly::baseline(&anomaly::same_time_of_day(&history, t, 3600), 4, 2.0)
                    {
                        a.usual = Some(b.center);
                        a.z = Some(anomaly::z(a.airborne as f64, &b));
                    }
                    out.push(a);
                }
                Err(FetchError::Status(429)) => {
                    return Err(TrackerError::Fetch(FetchError::Status(429)));
                }
                Err(e) => failures.push(format!("{}: {e}", place.name)),
            }
        }
        if out.is_empty() && !failures.is_empty() {
            return Err(TrackerError::Invalid(failures.join("; ")));
        }
        self.update(|b| b.air = out);
        Ok((!failures.is_empty()).then(|| failures.join("; ")))
    }

    async fn refresh_routes(&self, c: &TrackerConfig) -> Result<Option<String>, TrackerError> {
        let mut out = vec![];
        let mut failures = vec![];
        for r in &c.routes {
            let (Some(from), Some(to)) = (c.place(&r.from), c.place(&r.to)) else {
                continue;
            };
            let parsed = self
                .net
                .get_json(&world::route_url(from, to), API_MAX_BYTES)
                .await
                .map_err(|e| e.to_string())
                .and_then(|b| world::parse_route(&b).ok_or_else(|| "no route found".into()));
            match parsed {
                Ok((duration_s, distance_m, geometry)) => out.push(world::RouteInfo {
                    id: r.id.clone(),
                    name: r.name.clone(),
                    from: r.from.clone(),
                    to: r.to.clone(),
                    duration_s,
                    distance_m,
                    geometry,
                    fetched_at: now(),
                }),
                Err(e) => failures.push(format!("{}: {e}", r.name)),
            }
        }
        if out.is_empty() && !failures.is_empty() {
            return Err(TrackerError::Invalid(failures.join("; ")));
        }
        self.update(|b| b.routes = out);
        Ok((!failures.is_empty()).then(|| failures.join("; ")))
    }

    async fn refresh_quakes(&self, c: &TrackerConfig) -> Result<Option<String>, TrackerError> {
        let body = self.net.get_json(world::QUAKES_URL, API_MAX_BYTES).await?;
        let quakes = world::parse_quakes(&body, &c.places);
        self.update(|b| b.quakes = quakes);
        Ok(None)
    }

    // ------------------------------------------------------------ helpers

    pub async fn geocode(&self, query: &str) -> Result<Vec<world::PlaceCandidate>, TrackerError> {
        let q = query.trim();
        if q.is_empty() || q.chars().count() > 80 {
            return Err(TrackerError::Invalid(
                "Search for a place name of 1–80 characters".into(),
            ));
        }
        self.touch();
        let body = self
            .net
            .get_json(&world::geocode_url(q), API_MAX_BYTES)
            .await?;
        Ok(world::parse_geocode(&body))
    }

    /// A basemap tile, cached on disk so panning around the same area stays local.
    pub async fn tile(&self, z: u32, x: u32, y: u32) -> Result<Vec<u8>, TrackerError> {
        if z > 18 || x >= (1 << z) || y >= (1 << z) {
            return Err(TrackerError::Invalid("Tile out of range".into()));
        }
        let path: PathBuf = self
            .store
            .dir()
            .join("tiles")
            .join(z.to_string())
            .join(x.to_string())
            .join(format!("{y}.png"));
        if let Ok(meta) = tokio::fs::metadata(&path).await
            && meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age.as_secs() < TILE_MAX_AGE_SECS)
            && let Ok(bytes) = tokio::fs::read(&path).await
        {
            return Ok(bytes);
        }
        let _permit = self
            .tiles
            .acquire()
            .await
            .map_err(|_| TrackerError::Invalid("Tile service is shutting down".into()))?;
        // OpenStreetMap's standard tiles: keyless, cached here for 30 days as its tile
        // policy asks; the board darkens them for display.
        let url = format!("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
        let bytes = self.net.get_bytes(&url, TILE_MAX_BYTES).await?;
        if !bytes.starts_with(b"\x89PNG") {
            return Err(TrackerError::Fetch(FetchError::Parse(
                "not a PNG tile".into(),
            )));
        }
        if let Some(parent) = path.parent()
            && tokio::fs::create_dir_all(parent).await.is_ok()
        {
            let tmp = path.with_extension("part");
            if tokio::fs::write(&tmp, &bytes).await.is_ok() {
                let _ = tokio::fs::rename(&tmp, &path).await;
            }
        }
        Ok(bytes)
    }
}

/// Board fields that a source's refresh changes.
fn fields(source: Source) -> &'static [&'static str] {
    match source {
        Source::News => &["news", "topics"],
        Source::Quotes => &["quotes"],
        Source::Fx => &["fx"],
        Source::Odds => &["odds"],
        Source::Weather => &["conditions"],
        Source::Air => &["air"],
        Source::Routes => &["routes"],
        Source::Quakes => &["quakes"],
    }
}

fn signed(z: f64) -> String {
    format!("{}{:.1}σ", if z >= 0.0 { "+" } else { "−" }, z.abs())
}

/// Everything deviating from its own baseline, strongest first.
pub fn derive_anomalies(b: &Board) -> Vec<Anomaly> {
    let mut all = Vec::new();
    for q in &b.quotes {
        if let Some(z) = q.move_z {
            all.push(Anomaly {
                key: format!("quote:{}", q.symbol),
                module: "markets",
                label: q.name.clone(),
                detail: format!(
                    "{:+.2}% today, {} vs its usual daily move",
                    q.change_pct,
                    signed(z)
                ),
                z,
            });
        }
    }
    for c in &b.conditions {
        if let (Some(z), Some(d)) = (c.temp_z, c.temp_vs_week) {
            all.push(Anomaly {
                key: format!("temp:{}", c.place),
                module: "conditions",
                label: format!("{} temperature", c.name),
                detail: format!("{:+.1}°C vs this hour last week ({})", d, signed(z)),
                z,
            });
        }
        if let Some(z) = c.gust_z.filter(|z| *z > 0.0) {
            all.push(Anomaly {
                key: format!("gust:{}", c.place),
                module: "conditions",
                label: format!("{} wind", c.name),
                detail: format!(
                    "Gusts {:.0} km/h, {} vs this hour last week",
                    c.gusts.unwrap_or(0.0),
                    signed(z)
                ),
                z,
            });
        }
    }
    for a in &b.air {
        if let (Some(z), Some(usual)) = (a.z, a.usual) {
            all.push(Anomaly {
                key: format!("air:{}", a.place),
                module: "routes",
                label: format!("{} air traffic", a.name),
                detail: format!(
                    "{} aircraft overhead, usually {:.0} at this time ({})",
                    a.airborne,
                    usual,
                    signed(z)
                ),
                z,
            });
        }
    }
    for t in &b.topics {
        if let (Some(z), Some(usual)) = (t.z, t.usual) {
            all.push(Anomaly {
                key: format!("news:{}", t.topic),
                module: "news",
                label: format!("“{}” coverage", t.topic),
                detail: format!(
                    "{} stories in 6h, usually {:.0} ({})",
                    t.stories_6h,
                    usual,
                    signed(z)
                ),
                z,
            });
        }
    }
    // Event-type signals have no baseline; these scores rank them against the rest.
    for m in &b.odds {
        if let Some(pts) = m.change_24h_pts.filter(|p| p.abs() >= 10.0) {
            all.push(Anomaly {
                key: format!("odds:{}", m.slug),
                module: "odds",
                label: m.question.clone(),
                detail: format!("{:+.0} points in 24h", pts),
                z: pts / 5.0,
            });
        }
    }
    let t = now();
    for q in b.quakes.iter().filter(|q| t - q.time < 86_400) {
        let near = q.nearest.as_ref().filter(|n| n.km <= 300.0);
        if q.magnitude >= 6.5 || (near.is_some() && q.magnitude >= 4.0) {
            let place = near
                .map(|n| format!(", {:.0} km from {}", n.km, n.place))
                .unwrap_or_default();
            all.push(Anomaly {
                key: format!("quake:{}", q.id),
                module: "hazards",
                label: format!("M{:.1} earthquake", q.magnitude),
                detail: format!("{}{}", q.title, place),
                z: 2.0 + (q.magnitude - 4.0).max(0.0) / 2.0,
            });
        }
    }
    anomaly::rank(all, 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_names_round_trip() {
        for s in Source::ALL {
            let name = serde_json::to_value(s).unwrap();
            assert_eq!(Source::parse(name.as_str().unwrap()), Some(s));
        }
        assert_eq!(Source::parse("nope"), None);
    }

    #[test]
    fn config_changes_only_refresh_affected_sources() {
        let tracker = Tracker::with_store(Store::open_in_memory().unwrap()).unwrap();
        let mut c = tracker.config();
        c.layout[0].hidden = true;
        tracker.set_config(c.clone()).unwrap();
        assert!(tracker.next_due.lock().unwrap().is_empty());
        c.tickers.pop();
        tracker.set_config(c).unwrap();
        let due = tracker.next_due.lock().unwrap();
        assert_eq!(
            due.keys().copied().collect::<Vec<_>>(),
            vec![Source::Quotes]
        );
    }

    #[test]
    fn rejects_invalid_config_without_saving() {
        let tracker = Tracker::with_store(Store::open_in_memory().unwrap()).unwrap();
        let mut c = tracker.config();
        c.base_currency = "euro".into();
        assert!(tracker.set_config(c).is_err());
        assert_eq!(tracker.config().base_currency, "EUR");
    }

    #[test]
    fn anomalies_rank_market_moves_and_weather() {
        let mut b = Board::default();
        b.quotes.push(markets::Quote {
            symbol: "X".into(),
            name: "X Corp".into(),
            currency: "USD".into(),
            price: 110.0,
            previous_close: 100.0,
            change: 10.0,
            change_pct: 10.0,
            move_z: Some(5.0),
            day_high: None,
            day_low: None,
            spark: vec![],
            market_time: 0,
        });
        b.odds.push(odds::Market {
            slug: "s".into(),
            question: "Q?".into(),
            outcomes: vec![],
            change_24h_pts: Some(-15.0),
            volume_24h: None,
            ends: None,
            url: String::new(),
        });
        let a = derive_anomalies(&b);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].key, "quote:X");
        assert_eq!(a[1].z, -3.0);
    }

    #[tokio::test]
    async fn tile_rejects_out_of_range() {
        let tracker = Tracker::with_store(Store::open_in_memory().unwrap()).unwrap();
        assert!(tracker.tile(2, 4, 0).await.is_err());
        assert!(tracker.tile(19, 0, 0).await.is_err());
    }
}
