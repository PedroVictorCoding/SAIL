//! Round 13: the trade-signal run (universe, prices, earnings, signals, grading, guards).
use super::{CapitalService, ServiceError};
use crate::contracts::{AccountId, AsOf, InstrumentKind};
use crate::trade::{AssetClass, Bar, Instrument, SignalDay};
use crate::universe::{WatchChange, WatchItem};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

/// Hour and minute (UTC) of the daily run, after the US close.
pub const RUN_AT_UTC: (u32, u32) = (22, 30);

/// Earnings dates are refetched after this many days.
const EARNINGS_MAX_AGE_DAYS: i64 = 7;

/// Fewest bars with open/high/low before an instrument joins the universe.
const MIN_OHL_BARS: usize = 300;

/// `GET /signals/trade`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TradeView {
    /// The latest run's result, if any.
    pub day: Option<SignalDay>,
    /// When it ran.
    pub run_at: Option<DateTime<Utc>>,
    /// When the next scheduled run is.
    pub next_run_at: DateTime<Utc>,
    /// Why there's no run yet, or why the last one fell short.
    pub notes: Vec<String>,
    /// A run is in progress now (started from the page or the schedule).
    #[serde(default)]
    pub running: bool,
    /// Why the last run failed, if it did (cleared by the next successful run).
    #[serde(default)]
    pub last_error: Option<String>,
}

/// `GET /signals/trade/record`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TradeRecord {
    /// Published cards with their grades, newest first.
    pub cards: Vec<crate::store::GradedCard>,
    /// Totals over closed verified cards.
    pub totals: RecordTotals,
    /// Totals over closed candidate cards, kept apart from the verified record.
    #[serde(default)]
    pub candidate_totals: RecordTotals,
}

/// Totals over closed cards.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordTotals {
    /// Closed cards.
    pub closed: u32,
    /// Share that reached target 1.
    pub hit_target1: f64,
    /// Mean R after costs.
    pub mean_r: f64,
    /// Sum of R.
    pub cumulative_r: f64,
}

/// One instrument matched by an asset search.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SymbolSearchResult {
    /// The Yahoo symbol, e.g. `AAPL` or `^GSPC`.
    pub symbol: String,
    /// Display name.
    pub name: String,
    /// Exchange, as Yahoo labels it.
    pub exchange: String,
    /// Asset class used by the setup costs and correlation group.
    pub class: AssetClass,
}

/// A one-off signal check for an asset outside the daily universe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TradeSignalCheck {
    /// The Yahoo symbol checked.
    pub symbol: String,
    /// Display name.
    pub name: String,
    /// Exchange, when the search supplied one.
    pub exchange: Option<String>,
    /// Asset class used.
    pub class: AssetClass,
    /// Trading currency from the price series, when known.
    pub currency: Option<String>,
    /// Session of the latest bar used.
    pub session: Option<NaiveDate>,
    /// Latest close used.
    pub close: f64,
    /// Daily bars with open, high and low.
    pub bars_used: usize,
    /// Cards that publish for this instrument today.
    pub cards: Vec<crate::trade::SignalCard>,
    /// Setups that fired but did not publish, with the reason.
    pub suppressed: Vec<crate::trade::SuppressedSignal>,
    /// Verdicts for the instrument's asset group.
    pub verdicts: Vec<crate::trade::Verdict>,
    /// The owner already holds this symbol.
    pub held: bool,
    /// The symbol is already on the daily watchlist.
    pub on_watchlist: bool,
    /// The last closes, oldest first, for the mini chart.
    pub recent_closes: Vec<(NaiveDate, f64)>,
    /// Plain notes on the evidence and the limits of a one-off check.
    pub notes: Vec<String>,
}

/// The next scheduled run after `now`: the next weekday at [`RUN_AT_UTC`].
pub fn next_run_after(now: DateTime<Utc>) -> DateTime<Utc> {
    let mut day = now.date_naive();
    loop {
        let at = day
            .and_hms_opt(RUN_AT_UTC.0, RUN_AT_UTC.1, 0)
            .expect("valid time")
            .and_utc();
        if at > now && day.weekday().number_from_monday() <= 5 {
            return at;
        }
        day += Duration::days(1);
    }
}

/// Instrument key for a watchlist entry.
pub fn watch_key(symbol: &str) -> String {
    format!("watch:{}", symbol.to_ascii_uppercase())
}

/// Whether a Yahoo symbol is safe to send to the helpers.
fn validate_search_symbol(symbol: &str) -> Result<(), ServiceError> {
    let valid = !symbol.is_empty()
        && symbol.len() <= 24
        && symbol
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-=^".contains(&byte))
        && (symbol.as_bytes()[0].is_ascii_alphanumeric() || symbol.as_bytes()[0] == b'^');
    if valid {
        Ok(())
    } else {
        Err(ServiceError::BadRequest(format!(
            "{symbol:?} isn't a valid Yahoo symbol."
        )))
    }
}

/// Parses the optional asset class from the check request; stock by default.
fn parse_class(class: Option<&str>) -> Result<AssetClass, ServiceError> {
    match class.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(AssetClass::Stock),
        Some(value) => match value.to_ascii_lowercase().as_str() {
            "stock" => Ok(AssetClass::Stock),
            "etf" => Ok(AssetClass::Etf),
            "fx" => Ok(AssetClass::Fx),
            "index" => Ok(AssetClass::Index),
            "commodity" => Ok(AssetClass::Commodity),
            _ => Err(ServiceError::BadRequest(format!(
                "Unknown asset class {value}."
            ))),
        },
    }
}

/// Maps a Yahoo quote type to the class whose costs and evidence group apply.
fn asset_class_for(quote_type: &str) -> Option<AssetClass> {
    match quote_type.to_ascii_uppercase().as_str() {
        "EQUITY" => Some(AssetClass::Stock),
        "ETF" | "MUTUALFUND" => Some(AssetClass::Etf),
        "INDEX" => Some(AssetClass::Index),
        "CURRENCY" => Some(AssetClass::Fx),
        "FUTURE" | "CRYPTOCURRENCY" => Some(AssetClass::Commodity),
        _ => None,
    }
}

impl CapitalService {
    /// The current watchlist.
    pub fn watchlist(&self) -> Result<Vec<WatchItem>, ServiceError> {
        Ok(crate::universe::apply(&self.store.watch_changes()?))
    }

    /// Applies a watchlist change and returns the new list.
    pub fn change_watchlist(
        &self,
        change: WatchChange,
        now: DateTime<Utc>,
    ) -> Result<Vec<WatchItem>, ServiceError> {
        let change = match change {
            WatchChange::Add { mut item } => {
                item.symbol = item.symbol.trim().to_ascii_uppercase();
                item.yahoo = item.yahoo.trim().to_string();
                item.currency = item.currency.trim().to_ascii_uppercase();
                item.name = item.name.trim().to_string();
                crate::universe::validate(&item).map_err(ServiceError::BadRequest)?;
                WatchChange::Add { item }
            }
            WatchChange::Remove { symbol } => {
                let symbol = symbol.trim().to_ascii_uppercase();
                if !self.watchlist()?.iter().any(|w| w.symbol == symbol) {
                    return Err(ServiceError::BadRequest(format!(
                        "{symbol} isn't on the watchlist."
                    )));
                }
                WatchChange::Remove { symbol }
            }
        };
        self.store.record_watch_change(&change, now)?;
        self.watchlist()
    }

    /// The latest run, with the next scheduled time.
    pub fn trade_view(&self, account: &AccountId) -> Result<TradeView, ServiceError> {
        let now = Utc::now();
        let latest = self.store.latest_trade_run(account)?;
        let mut notes = Vec::new();
        if latest.is_none() {
            notes.push(if self.prices_configured {
                "No run yet. Press Run now, or wait for the daily run after the US close.".into()
            } else {
                "Price fetching is off (SAIL_PRICES_SOURCE), so signals can't be computed.".into()
            });
        }
        Ok(TradeView {
            run_at: latest.as_ref().map(|(at, _)| *at),
            day: latest.map(|(_, day)| day),
            next_run_at: next_run_after(now),
            notes,
            running: self.trade_running.try_lock().is_err(),
            last_error: self.trade_last_error.lock().ok().and_then(|e| e.clone()),
        })
    }

    /// Searches Yahoo for instruments the owner can check for a potential signal.
    pub async fn search_trade_symbols(
        &self,
        query: &str,
    ) -> Result<Vec<SymbolSearchResult>, ServiceError> {
        self.ensure_asset_search()?;
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 64 {
            return Err(ServiceError::BadRequest(
                "Search needs between 1 and 64 characters.".into(),
            ));
        }
        let matches = self
            .prices
            .search(query)
            .await
            .map_err(|error| ServiceError::BadRequest(error.to_string()))?;
        let mut results = Vec::new();
        for item in matches {
            let Some(class) = asset_class_for(&item.quote_type) else {
                continue;
            };
            results.push(SymbolSearchResult {
                symbol: item.symbol,
                name: item.name,
                exchange: item.exchange,
                class,
            });
            if results.len() >= 10 {
                break;
            }
        }
        Ok(results)
    }

    /// Checks one asset against the latest published scoreboard, even when it isn't held
    /// or on the watchlist. The price history is fetched for this look only and isn't
    /// stored; adding the asset to the watchlist includes it in later runs.
    pub async fn check_trade_signal(
        &self,
        account: &AccountId,
        symbol: &str,
        yahoo: Option<&str>,
        name: Option<&str>,
        exchange: Option<&str>,
        class: Option<&str>,
    ) -> Result<TradeSignalCheck, ServiceError> {
        self.ensure_asset_search()?;
        let symbol = symbol.trim();
        let yahoo = yahoo.unwrap_or(symbol).trim();
        validate_search_symbol(symbol)?;
        validate_search_symbol(yahoo)?;
        let class = parse_class(class)?;
        let now = Utc::now();
        let (run_at, day) = self.store.latest_trade_run(account)?.ok_or_else(|| {
            ServiceError::BadRequest(
                "Run the signals once before checking an asset: the scoreboard supplies the evidence."
                    .into(),
            )
        })?;
        let series = self
            .prices
            .daily_as(symbol, yahoo)
            .await
            .map_err(|error| ServiceError::BadRequest(error.to_string()))?;
        let bars: Vec<Bar> = series
            .bars
            .iter()
            .filter_map(|bar| {
                Some(Bar {
                    session: bar.session,
                    open: bar.open?.to_f64()?,
                    high: bar.high?.to_f64()?,
                    low: bar.low?.to_f64()?,
                    close: bar.close.to_f64()?,
                })
            })
            .filter(|bar| bar.open > 0.0 && bar.high >= bar.low && bar.close > 0.0)
            .collect();
        if bars.len() < MIN_OHL_BARS {
            return Err(ServiceError::BadRequest(format!(
                "{symbol} has only {} daily bars with open/high/low; at least {MIN_OHL_BARS} are needed to check its setups.",
                bars.len()
            )));
        }

        let yahoo_owned = yahoo.to_string();
        let mut earnings = Vec::new();
        if matches!(class, AssetClass::Stock | AssetClass::Etf) {
            let _ = self
                .refresh_earnings_with_timeout(
                    std::slice::from_ref(&yahoo_owned),
                    std::time::Duration::from_secs(20),
                )
                .await;
            earnings = self.earnings_for(Some(&yahoo_owned))?;
        }

        let insights = self.insights(account, AsOf::now(), now)?;
        let account_value = insights
            .account
            .as_ref()
            .and_then(|account| account.account_value.amount.to_f64());
        let risk_pct = self
            .policy(account)?
            .current
            .limits
            .risk_per_trade_pct
            .to_f64()
            .unwrap_or(0.5);
        let account_currency = self.account_currency(account)?;
        let watch = self.watchlist()?;
        let mut fx_closes = FxCloses::new();
        for item in &watch {
            if item.class != AssetClass::Fx {
                continue;
            }
            let key = watch_key(&item.symbol);
            if let Some(stored) = self.bars_for(&key, now)?
                && let Some(last) = stored.last()
            {
                fx_closes.insert(item.symbol.to_ascii_uppercase(), last.close);
            }
        }

        let checked_key = format!("check:{}", symbol.to_ascii_uppercase());
        let checked_currency = series
            .currency
            .as_ref()
            .map(|currency| currency.as_str().to_string());
        let earnings_known = !earnings.is_empty();
        let checked = Instrument {
            key: checked_key.clone(),
            symbol: symbol.to_string(),
            class,
            foreign_currency: checked_currency
                .as_deref()
                .is_some_and(|currency| currency != account_currency),
            bars,
            earnings,
            evidence_only: false,
            membership: Vec::new(),
        };
        let fx = |instrument: &Instrument| -> Option<f64> {
            if instrument.key != checked_key {
                return None;
            }
            let currency = checked_currency.as_deref()?;
            fx_rate(currency, &account_currency, &fx_closes)
        };
        let held = self
            .held_instruments(account)?
            .iter()
            .any(|(_, held_symbol)| held_symbol.eq_ignore_ascii_case(symbol));
        let held_fn = |key: &str| -> Option<String> {
            if key == checked_key && held {
                Some(String::new())
            } else {
                None
            }
        };
        let check = crate::trade::check_instrument(
            &checked,
            &day.scoreboard,
            account_value,
            risk_pct,
            &fx,
            &held_fn,
        );

        let group = crate::trade::group_of(class);
        let verdicts: Vec<crate::trade::Verdict> = day
            .scoreboard
            .iter()
            .filter(|verdict| verdict.group == group)
            .cloned()
            .collect();
        let on_watchlist = watch.iter().any(|item| {
            item.symbol.eq_ignore_ascii_case(symbol) || item.yahoo.eq_ignore_ascii_case(yahoo)
        });
        let last = checked
            .bars
            .last()
            .expect("the checked instrument has at least one bar");
        let recent_closes: Vec<(NaiveDate, f64)> = checked
            .bars
            .iter()
            .rev()
            .take(60)
            .rev()
            .map(|bar| (bar.session, bar.close))
            .collect();

        let mut notes = vec![format!(
            "Checked against the scoreboard from the run at {} (session {}). This one-off look does not add {} to the daily universe.",
            run_at.format("%Y-%m-%d %H:%M UTC"),
            day.session,
            symbol
        )];
        if verdicts.is_empty() {
            notes.push(format!(
                "The latest run has no {group} verdicts; run the signals again before relying on this check."
            ));
        } else if !verdicts.iter().any(|verdict| verdict.publishes) {
            notes.push(format!(
                "No {group} setup has passed validation, so a card cannot publish for {symbol} yet."
            ));
        }
        if !on_watchlist {
            notes.push(format!(
                "Add {symbol} to the watchlist to include it in the next daily run."
            ));
        }
        if class == AssetClass::Stock && !earnings_known {
            notes.push(format!(
                "No earnings dates were available for {symbol}, so the earnings filter could not be applied."
            ));
        }
        if check.session < day.session {
            notes.push(format!(
                "{symbol}'s latest bar is {} and older than the run session {}; treat any plan as historical.",
                check.session, day.session
            ));
        }
        notes.push(
            "Prices are Yahoo daily bars; a stop and a target in the same session count as the stop, and costs are assumptions."
                .to_string(),
        );

        Ok(TradeSignalCheck {
            symbol: symbol.to_string(),
            name: name
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(symbol)
                .to_string(),
            exchange: exchange
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.chars().take(80).collect()),
            class,
            currency: checked_currency,
            session: Some(last.session),
            close: last.close,
            bars_used: checked.bars.len(),
            cards: check.cards,
            suppressed: check.suppressed,
            verdicts,
            held,
            on_watchlist,
            recent_closes,
            notes,
        })
    }

    fn ensure_asset_search(&self) -> Result<(), ServiceError> {
        if !self.prices_configured {
            return Err(ServiceError::BadRequest(
                crate::market::PriceSourceError::NotConfigured.to_string(),
            ));
        }
        if self.prices.source_id() != "yfinance" {
            return Err(ServiceError::BadRequest(
                "Asset search needs Yahoo price fetching (SAIL_PRICES_SOURCE=yfinance).".into(),
            ));
        }
        Ok(())
    }

    /// Published cards with their grades and totals.
    pub fn trade_record(&self, account: &AccountId) -> Result<TradeRecord, ServiceError> {
        let cards = self.store.trade_record(account)?;
        let totals = record_totals(&cards, crate::trade::Tier::Proven);
        let candidate_totals = record_totals(&cards, crate::trade::Tier::Candidate);
        Ok(TradeRecord {
            cards,
            totals,
            candidate_totals,
        })
    }

    /// Protective stops for held instruments with enough bars.
    pub fn trade_guards(
        &self,
        account: &AccountId,
    ) -> Result<Vec<crate::trade::HoldingGuard>, ServiceError> {
        let mut out = Vec::new();
        for instrument in self.held_universe(account, Utc::now())? {
            if let Some(guard) = crate::trade::guard(&instrument) {
                out.push(guard);
            }
        }
        Ok(out)
    }

    /// Bars for `key` up to `known_by`, using open/high/low where stored. `None` when too
    /// few bars carry them.
    fn bars_for(
        &self,
        key: &str,
        known_by: DateTime<Utc>,
    ) -> Result<Option<Vec<Bar>>, ServiceError> {
        let observations = self.store.prices(key, known_by)?;
        let bars: Vec<Bar> = observations
            .iter()
            .filter_map(|o| {
                let ohl = o.ohl.as_ref()?;
                Some(Bar {
                    session: o.session,
                    open: ohl.open.to_f64()?,
                    high: ohl.high.to_f64()?,
                    low: ohl.low.to_f64()?,
                    close: o.close.to_f64()?,
                })
            })
            .filter(|b| b.open > 0.0 && b.high >= b.low && b.close > 0.0)
            .collect();
        Ok((bars.len() >= MIN_OHL_BARS).then_some(bars))
    }

    fn earnings_for(&self, yahoo: Option<&str>) -> Result<Vec<NaiveDate>, ServiceError> {
        Ok(match yahoo {
            Some(y) => self.store.earnings(y)?.map(|(_, d)| d).unwrap_or_default(),
            None => Vec::new(),
        })
    }

    /// Held instruments as signal-universe instruments.
    fn held_universe(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<Vec<Instrument>, ServiceError> {
        let projection = self.portfolio(account, AsOf::now())?;
        let account_currency = self.account_currency(account)?;
        let mut out = Vec::new();
        for position in &projection.positions {
            let key = position.instrument.key();
            let Some(bars) = self.bars_for(&key, now)? else {
                continue;
            };
            let class = match position.instrument.kind {
                InstrumentKind::Etf => AssetClass::Etf,
                _ => AssetClass::Stock,
            };
            let currency = self.instrument_currency(&key, now)?;
            let yahoo = crate::market::yahoo_symbol(&position.instrument.symbol);
            out.push(Instrument {
                key,
                symbol: position.instrument.symbol.clone(),
                class,
                foreign_currency: currency.as_deref().is_some_and(|c| c != account_currency),
                bars,
                earnings: if class == AssetClass::Stock {
                    self.earnings_for(yahoo.as_deref())?
                } else {
                    Vec::new()
                },
                evidence_only: false,
                membership: Vec::new(),
            });
        }
        Ok(out)
    }

    pub(super) fn account_currency(&self, account: &AccountId) -> Result<String, ServiceError> {
        Ok(self
            .insights(account, AsOf::now(), Utc::now())?
            .account
            .map(|a| a.account_value.currency.as_str().to_string())
            .unwrap_or_else(|| "EUR".into()))
    }

    fn instrument_currency(
        &self,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, ServiceError> {
        Ok(self
            .store
            .prices(key, now)?
            .last()
            .and_then(|o| o.currency.as_ref().map(|c| c.as_str().to_string())))
    }

    /// Fetches prices for the watchlist (not the holdings, which `refresh_prices`
    /// covers), at the source's rate limit. Returns the symbols that failed.
    async fn refresh_watch_prices(
        &self,
        items: &[WatchItem],
        held_symbols: &[String],
    ) -> Result<Vec<String>, ServiceError> {
        let mut failed = Vec::new();
        for item in items {
            if held_symbols
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&item.symbol))
            {
                continue;
            }
            match self.prices.daily_as(&item.symbol, &item.yahoo).await {
                Ok(series) => {
                    let store = self.store.clone();
                    let key = watch_key(&item.symbol);
                    tokio::task::spawn_blocking(move || {
                        store.record_prices(&key, &series, Utc::now())
                    })
                    .await
                    .map_err(|_| ServiceError::Store(crate::store::StoreError::Poisoned))??;
                }
                Err(crate::market::PriceSourceError::Refused(_))
                | Err(crate::market::PriceSourceError::Blocked) => {
                    failed.push(item.symbol.clone());
                    break;
                }
                Err(_) => failed.push(item.symbol.clone()),
            }
        }
        Ok(failed)
    }

    /// Refetches earnings dates for stocks whose snapshot is missing or older than a week.
    async fn refresh_earnings(&self, yahoo: &[String]) -> Result<(), ServiceError> {
        self.refresh_earnings_with_timeout(yahoo, std::time::Duration::from_secs(600))
            .await
    }

    async fn refresh_earnings_with_timeout(
        &self,
        yahoo: &[String],
        timeout: std::time::Duration,
    ) -> Result<(), ServiceError> {
        let now = Utc::now();
        let mut stale = Vec::new();
        for symbol in yahoo {
            let fresh = self
                .store
                .earnings(symbol)?
                .is_some_and(|(at, _)| (now - at).num_days() < EARNINGS_MAX_AGE_DAYS);
            if !fresh {
                stale.push(symbol.clone());
            }
        }
        if stale.is_empty() {
            return Ok(());
        }
        let python = std::env::var("SAIL_YFINANCE_PYTHON")
            .unwrap_or_else(|_| crate::market::yahoo::DEFAULT_PYTHON.to_string());
        let script = std::env::var("SAIL_EARNINGS_SCRIPT")
            .unwrap_or_else(|_| "scripts/prices/earnings.py".to_string());
        for chunk in stale.chunks(100) {
            let mut command = tokio::process::Command::new(&python);
            command
                .arg("-I")
                .arg(&script)
                .args(chunk)
                .env_clear()
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            for var in [
                "PATH",
                "HOME",
                "LANG",
                "SSL_CERT_FILE",
                "HTTPS_PROXY",
                "https_proxy",
            ] {
                if let Ok(value) = std::env::var(var) {
                    command.env(var, value);
                }
            }
            command.env("XDG_CACHE_HOME", crate::market::yahoo::DEFAULT_CACHE);
            let Ok(Ok(output)) = tokio::time::timeout(timeout, command.output()).await else {
                tracing::warn!("earnings helper did not finish");
                continue;
            };
            #[derive(Deserialize)]
            struct Out {
                #[serde(default)]
                dates: std::collections::BTreeMap<String, Vec<NaiveDate>>,
            }
            let Ok(out) = serde_json::from_slice::<Out>(&output.stdout) else {
                tracing::warn!("earnings helper output unreadable");
                continue;
            };
            for (symbol, dates) in out.dates {
                self.store.record_earnings(&symbol, &dates, Utc::now())?;
            }
        }
        Ok(())
    }

    /// Starts a run in the background and returns at once. A run takes minutes (one
    /// price request a second), longer than proxies such as a Cloudflare tunnel keep a
    /// request open, so the page polls [`CapitalService::trade_view`] while `running`.
    /// Returns false when a run is already in progress.
    pub fn start_trade_run(self: &std::sync::Arc<Self>, account: AccountId) -> bool {
        if self.trade_running.try_lock().is_err() {
            return false;
        }
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(e) = service.run_trade_signals(&account).await {
                tracing::warn!(error = %e, "trade signals run failed");
            }
        });
        true
    }

    /// Runs the whole signal pipeline now: prices for holdings and watchlist, earnings,
    /// signals, and grading of earlier cards. Only one run at a time. Its outcome is kept
    /// for [`TradeView::last_error`].
    pub async fn run_trade_signals(&self, account: &AccountId) -> Result<TradeView, ServiceError> {
        let result = {
            let _running = self.trade_running.lock().await;
            self.run_trade_signals_locked(account).await
        };
        if let Ok(mut last) = self.trade_last_error.lock() {
            *last = result.as_ref().err().map(|e| e.to_string());
        }
        result.and_then(|_| self.trade_view(account))
    }

    async fn run_trade_signals_locked(&self, account: &AccountId) -> Result<(), ServiceError> {
        if !self.prices_configured {
            return Ok(());
        }
        let started = Utc::now();
        let watch = self.watchlist()?;
        let held = self.held_instruments(account)?;
        let held_symbols: Vec<String> = held.iter().map(|(_, s)| s.clone()).collect();
        let _ = self.refresh_prices(Some(account)).await?;
        let failed = self.refresh_watch_prices(&watch, &held_symbols).await?;

        let mut stock_yahoo: Vec<String> = watch
            .iter()
            .filter(|w| w.class == AssetClass::Stock)
            .map(|w| w.yahoo.clone())
            .collect();
        stock_yahoo.extend(
            held_symbols
                .iter()
                .filter_map(|s| crate::market::yahoo_symbol(s)),
        );
        stock_yahoo.sort();
        stock_yahoo.dedup();
        self.refresh_earnings(&stock_yahoo).await?;

        let now = Utc::now();
        let mut universe = self.held_universe(account, now)?;
        let account_currency = self.account_currency(account)?;
        let mut fx_closes = FxCloses::new();
        let mut currencies = std::collections::BTreeMap::new();
        for item in &watch {
            if held_symbols
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&item.symbol))
            {
                continue;
            }
            let key = watch_key(&item.symbol);
            let Some(bars) = self.bars_for(&key, now)? else {
                continue;
            };
            if item.class == AssetClass::Fx
                && let Some(last) = bars.last()
            {
                fx_closes.insert(item.symbol.to_ascii_uppercase(), last.close);
            }
            currencies.insert(key.clone(), item.currency.clone());
            universe.push(Instrument {
                key,
                symbol: item.symbol.clone(),
                class: item.class,
                foreign_currency: item.currency != account_currency,
                bars,
                earnings: if item.class == AssetClass::Stock {
                    self.earnings_for(Some(&item.yahoo))?
                } else {
                    Vec::new()
                },
                evidence_only: false,
                membership: Vec::new(),
            });
        }
        for instrument in &universe {
            if !currencies.contains_key(&instrument.key)
                && let Some(c) = self.instrument_currency(&instrument.key, now)?
            {
                currencies.insert(instrument.key.clone(), c);
            }
        }

        let insights = self.insights(account, AsOf::now(), now)?;
        let account_value = insights
            .account
            .as_ref()
            .and_then(|a| a.account_value.amount.to_f64());
        let risk_pct = self
            .policy(account)?
            .current
            .limits
            .risk_per_trade_pct
            .to_f64()
            .unwrap_or(0.5);
        let held_desc: std::collections::BTreeMap<String, String> = insights
            .allocation
            .iter()
            .map(|slice| {
                let share = slice.share.to_f64().unwrap_or(0.0) * 100.0;
                (
                    slice.instrument_key.clone(),
                    format!("You hold this ({share:.1}% of your owned value)"),
                )
            })
            .collect();
        let fx = |instrument: &Instrument| -> Option<f64> {
            let currency = currencies.get(&instrument.key)?;
            fx_rate(currency, &account_currency, &fx_closes)
        };
        let held_fn = |key: &str| held_desc.get(key).cloned();
        // Round 19: research names add evidence; bars with a likely unadjusted split never
        // do. Replays come from the cache where the past is unchanged.
        universe.extend(self.research_universe(now)?);
        universe.retain(|instrument| crate::trade::quality::usable(&instrument.bars));
        let cached = self.cached_replays(&universe)?;
        let live = self.live_trades(account)?;
        let mut day = crate::trade::signal_day_with(&crate::trade::RunInputs {
            universe: &universe,
            replays: &cached.replays,
            live: &live,
            account_value,
            risk_pct,
            fx_to_account: &fx,
            held: &held_fn,
        });
        if let Some(stats) = day.run_stats.as_mut() {
            stats.replay_cache_hits = cached.hits;
            stats.replay_cache_misses = cached.misses;
            stats.run_ms = u64::try_from((Utc::now() - started).num_milliseconds()).unwrap_or(0);
        }
        if !failed.is_empty() {
            day.notes.push(format!(
                "Prices could not be fetched for {}.",
                failed.join(", ")
            ));
        }
        self.store.record_trade_run(account, &day, now)?;
        self.grade_open_cards(account, now)?;
        Ok(())
    }

    /// Grades published cards that are still open, against bars after their session.
    fn grade_open_cards(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<(), ServiceError> {
        for graded in self.store.trade_record(account)? {
            if graded.status != "open" {
                continue;
            }
            let card = &graded.card;
            let Some(bars) = self.bars_for(&card.key, now)? else {
                continue;
            };
            let after: Vec<Bar> = bars
                .into_iter()
                .filter(|b| b.session > card.signal_session)
                .collect();
            let Some(first) = after.first() else {
                continue;
            };
            if first.open < card.entry_zone.0 || first.open > card.entry_zone.1 {
                self.store
                    .record_trade_grade(account, &card.id, "unfilled", None, now)?;
                continue;
            }
            let costs = crate::trade::Costs::default_for(card.class);
            if let Some(trade) = crate::trade::grade(card, &after, costs) {
                self.store
                    .record_trade_grade(account, &card.id, "closed", Some(&trade), now)?;
            }
        }
        Ok(())
    }

    /// Starts the daily run (weekdays at [`RUN_AT_UTC`]) when price fetching is on.
    pub(super) fn start_trade_schedule(self: &std::sync::Arc<Self>) {
        if !self.prices_configured {
            tracing::info!("Trade signals schedule off: price fetching is off");
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            loop {
                let next = next_run_after(Utc::now());
                let wait = (next - Utc::now()).to_std().unwrap_or_default();
                tokio::time::sleep(wait).await;
                let accounts = match service.accounts() {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!(error = %e, "trade signals: no accounts");
                        continue;
                    }
                };
                for summary in accounts {
                    match service.run_trade_signals(&summary.account).await {
                        Ok(view) => tracing::info!(
                            cards = view.day.as_ref().map(|d| d.cards.len()).unwrap_or(0),
                            "trade signals run finished"
                        ),
                        Err(e) => tracing::warn!(error = %e, "trade signals run failed"),
                    }
                }
            }
        });
    }
}

/// Totals over the closed cards of one tier.
fn record_totals(cards: &[crate::store::GradedCard], tier: crate::trade::Tier) -> RecordTotals {
    let closed: Vec<&crate::trade::Trade> = cards
        .iter()
        .filter(|c| c.status == "closed" && c.card.tier == tier)
        .filter_map(|c| c.graded.as_ref())
        .collect();
    let n = closed.len();
    if n == 0 {
        return RecordTotals::default();
    }
    let sum: f64 = closed.iter().map(|t| t.r_multiple).sum();
    let hits = closed
        .iter()
        .filter(|t| !matches!(t.exit, crate::trade::Exit::Stop | crate::trade::Exit::Time))
        .count();
    RecordTotals {
        closed: n as u32,
        hit_target1: hits as f64 / n as f64,
        mean_r: sum / n as f64,
        cumulative_r: sum,
    }
}

/// Latest FX closes by XTB pair name (e.g. `EURUSD`).
pub type FxCloses = std::collections::BTreeMap<String, f64>;

/// Units of the account currency per unit of `currency`, from the watchlist's FX
/// closes. Pence (GBX) are a hundredth of a pound.
pub fn fx_rate(currency: &str, account_currency: &str, closes: &FxCloses) -> Option<f64> {
    let currency = currency.to_ascii_uppercase();
    if currency == "GBX" {
        return fx_rate("GBP", account_currency, closes).map(|r| r / 100.0);
    }
    if currency == account_currency {
        return Some(1.0);
    }
    let direct = |base: &str, quote: &str| closes.get(&format!("{base}{quote}")).copied();
    // account/currency quoted directly (e.g. EURUSD): 1 currency = 1/rate account.
    if let Some(rate) = direct(account_currency, &currency) {
        return Some(1.0 / rate);
    }
    if let Some(rate) = direct(&currency, account_currency) {
        return Some(rate);
    }
    // Through the dollar.
    let usd = if account_currency == "USD" {
        Some(1.0)
    } else {
        direct(account_currency, "USD")
            .map(|r| 1.0 / r)
            .or_else(|| direct("USD", account_currency))
    }?;
    if currency == "USD" {
        return Some(usd);
    }
    direct("USD", &currency)
        .map(|r| usd / r)
        .or_else(|| direct(&currency, "USD").map(|r| usd * r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn fx_rates_go_direct_inverse_or_through_the_dollar() {
        let closes: FxCloses = [
            ("EURUSD".to_string(), 1.25),
            ("USDJPY".to_string(), 150.0),
            ("EURGBP".to_string(), 0.8),
            ("AUDUSD".to_string(), 0.5),
        ]
        .into_iter()
        .collect();
        let rate = |c: &str| fx_rate(c, "EUR", &closes).unwrap();
        assert_eq!(rate("EUR"), 1.0);
        assert!((rate("USD") - 0.8).abs() < 1e-12);
        assert!((rate("GBP") - 1.25).abs() < 1e-12);
        assert!((rate("GBX") - 0.0125).abs() < 1e-12);
        assert!((rate("JPY") - 0.8 / 150.0).abs() < 1e-12);
        assert!((rate("AUD") - 0.4).abs() < 1e-12);
        assert_eq!(fx_rate("CHF", "EUR", &closes), None);
    }

    #[test]
    fn the_next_run_skips_weekends_and_past_times() {
        // Friday 2026-11-13 23:00 UTC -> Monday 2026-11-16 22:30.
        let fri = Utc.with_ymd_and_hms(2026, 11, 13, 23, 0, 0).unwrap();
        assert_eq!(
            next_run_after(fri),
            Utc.with_ymd_and_hms(2026, 11, 16, 22, 30, 0).unwrap()
        );
        let mon = Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        assert_eq!(
            next_run_after(mon),
            Utc.with_ymd_and_hms(2026, 11, 16, 22, 30, 0).unwrap()
        );
    }

    #[test]
    fn yahoo_quote_types_map_to_the_signal_groups() {
        assert_eq!(asset_class_for("EQUITY"), Some(AssetClass::Stock));
        assert_eq!(asset_class_for("etf"), Some(AssetClass::Etf));
        assert_eq!(asset_class_for("MUTUALFUND"), Some(AssetClass::Etf));
        assert_eq!(asset_class_for("INDEX"), Some(AssetClass::Index));
        assert_eq!(asset_class_for("CURRENCY"), Some(AssetClass::Fx));
        assert_eq!(asset_class_for("FUTURE"), Some(AssetClass::Commodity));
        assert_eq!(
            asset_class_for("CRYPTOCURRENCY"),
            Some(AssetClass::Commodity)
        );
        assert_eq!(asset_class_for("BOND"), None);
    }

    #[test]
    fn search_symbols_and_classes_are_validated() {
        assert!(validate_search_symbol("AAPL").is_ok());
        assert!(validate_search_symbol("^GSPC").is_ok());
        assert!(validate_search_symbol("BRK-B").is_ok());
        assert!(validate_search_symbol("").is_err());
        assert!(validate_search_symbol("A B").is_err());
        assert!(validate_search_symbol("-AAPL").is_err());
        assert_eq!(parse_class(None).unwrap(), AssetClass::Stock);
        assert_eq!(parse_class(Some("etf")).unwrap(), AssetClass::Etf);
        assert!(parse_class(Some("bond")).is_err());
    }
}
