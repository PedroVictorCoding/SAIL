//! SEC research: filings for owned instruments, identifier maps, and the background
//! refresh schedule.

use super::*;

/// What the last SEC refresh did, for `/sec/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecStatus {
    /// Whether `SAIL_SEC_USER_AGENT` is set. Without it SAIL makes no SEC requests.
    pub configured: bool,
    /// When the last refresh finished.
    pub last_refresh_at: Option<DateTime<Utc>>,
    /// The last refresh's error, owner-safe, if any.
    pub last_error: Option<String>,
    /// Issuers asked about in the last refresh.
    pub issuers_checked: u32,
    /// Filings listed in the responses.
    pub filings_seen: u32,
    /// New evidence stored (including revisions).
    pub new_evidence: u32,
    /// When the ticker map in use was fetched.
    pub map_fetched_at: Option<DateTime<Utc>>,
}

/// One owned position and how it links to SEC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchView {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// The link.
    #[serde(rename = "match")]
    pub matched: IssuerMatch,
}

/// The `/filings` reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingsView {
    /// Filings that touch a holding, newest first.
    pub items: Vec<WatchItem>,
    /// How each owned position links to SEC.
    pub matches: Vec<MatchView>,
    /// The bounds used.
    pub as_of: AsOf,
}

/// Seconds between warm-up passes.
const WARMUP_EVERY_SECS: u64 = 300;

impl CapitalService {
    /// How each owned position links to an issuer, using the ticker map SAIL had at
    /// `known_by`.
    fn matches(
        &self,
        projection: &Projection,
        known_by: DateTime<Utc>,
    ) -> Result<Vec<MatchView>, ServiceError> {
        let index = self.store.ticker_index(known_by)?;
        Ok(projection
            .positions
            .iter()
            .map(|p| MatchView {
                instrument_key: p.instrument.key(),
                symbol: p.instrument.symbol.clone(),
                matched: match &index {
                    Some(index) => crate::exposure::match_issuer(&p.instrument, index),
                    None => IssuerMatch::Unresolved {
                        reason: "SEC's ticker map hasn't been fetched yet".to_string(),
                    },
                },
            })
            .collect())
    }

    /// The filings that touch `account`'s holdings, as known at `as_of`.
    pub fn filings(&self, account: &AccountId, as_of: AsOf) -> Result<FilingsView, ServiceError> {
        let projection = self.portfolio(account, as_of)?;
        let matches = self.matches(&projection, as_of.known_by)?;
        let mut ciks: Vec<Cik> = matches
            .iter()
            .filter_map(|m| match &m.matched {
                IssuerMatch::Matched { cik, .. } => Some(*cik),
                _ => None,
            })
            .collect();
        ciks.sort();
        ciks.dedup();
        let evidence = self.store.evidence_for(&ciks)?;
        let pairs: Vec<(String, IssuerMatch)> = matches
            .iter()
            .map(|m| (m.instrument_key.clone(), m.matched.clone()))
            .collect();
        let items = crate::exposure::watch_items(&projection, &pairs, &evidence, as_of);
        Ok(FilingsView {
            items,
            matches,
            as_of,
        })
    }

    /// The last refresh's status.
    pub fn sec_status(&self) -> SecStatus {
        self.sec_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Asks SEC about the issuers `account` holds now (every account when `None`) and
    /// appends what it lists. Only one refresh runs at a time; a second caller waits and
    /// then runs its own.
    pub async fn refresh_sec(&self, account: Option<&AccountId>) -> SecStatus {
        let _running = self.sec_refreshing.lock().await;
        let now = Utc::now();
        let mut status = SecStatus {
            configured: self.sec_configured,
            map_fetched_at: self.sec_status().map_fetched_at,
            ..SecStatus::default()
        };
        if !self.sec_configured {
            status.last_error = Some(SourceError::NotConfigured.to_string());
            return self.finish(status, now);
        }
        match self.refresh_inner(account, now, &mut status).await {
            Ok(()) => {}
            Err(message) => status.last_error = Some(message),
        }
        self.finish(status, now)
    }

    fn finish(&self, mut status: SecStatus, now: DateTime<Utc>) -> SecStatus {
        status.last_refresh_at = Some(now);
        if let Ok(mut slot) = self.sec_status.lock() {
            *slot = status.clone();
        }
        status
    }

    async fn refresh_inner(
        &self,
        account: Option<&AccountId>,
        now: DateTime<Utc>,
        status: &mut SecStatus,
    ) -> Result<(), String> {
        // 1. The ticker map, reused while it is fresh. A fetched map is used as fetched: its
        // `fetched_at` is stamped during the request, after `now`.
        let stored = self.store.ticker_index(now).map_err(|e| e.to_string())?;
        let index = match stored {
            Some(i) if now - i.fetched_at < Duration::hours(TICKER_MAP_MAX_AGE_HOURS) => i,
            _ => {
                let index = self.edgar.ticker_index().await.map_err(|e| e.to_string())?;
                self.store
                    .record_ticker_index(&index)
                    .map_err(|e| e.to_string())?;
                index
            }
        };
        status.map_fetched_at = Some(index.fetched_at);

        // 2. The issuers held now.
        let accounts = match account {
            Some(a) => vec![a.clone()],
            None => self
                .accounts()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|a| a.account)
                .collect(),
        };
        let mut ciks: Vec<Cik> = Vec::new();
        for account in &accounts {
            let projection = self
                .portfolio(account, AsOf::now())
                .map_err(|e| e.to_string())?;
            for position in &projection.positions {
                if let IssuerMatch::Matched { cik, .. } =
                    crate::exposure::match_issuer(&position.instrument, &index)
                {
                    ciks.push(cik);
                }
            }
        }
        ciks.sort();
        ciks.dedup();
        ciks.truncate(crate::evidence::MAX_ISSUERS_PER_REFRESH);

        // 3. Each issuer's filings. A refusal stops the run so SAIL backs off; other
        //    errors are recorded and the next issuer is tried.
        let mut last_error = None;
        for cik in ciks {
            status.issuers_checked += 1;
            match self.edgar.issuer_filings(cik).await {
                Ok(filings) => {
                    let EvidenceOutcome { seen, added, .. } = self
                        .store
                        .record_filings(&filings, Utc::now())
                        .map_err(|e| e.to_string())?;
                    status.filings_seen += seen;
                    status.new_evidence += added;
                }
                Err(SourceError::Refused(code)) => {
                    return Err(SourceError::Refused(code).to_string());
                }
                Err(e) => last_error = Some(format!("CIK {cik}: {e}")),
            }
        }
        match last_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Starts the background SEC refresh when SEC is configured: every
    /// `$SAIL_SEC_REFRESH_HOURS` (1–168, default 12), first run a minute after start.
    /// Keeps the slow analysis warm in the background so the page doesn't compute it on
    /// first open: the track record (evaluation, signal skill, outlook replays) and the
    /// dividend inputs for every account. Each pass is a few cheap version checks when
    /// nothing changed; after an import or a price refresh it recomputes once.
    fn start_warmup(self: &std::sync::Arc<Self>) {
        let service = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            loop {
                let svc = service.clone();
                let started = std::time::Instant::now();
                let _ = tokio::task::spawn_blocking(move || svc.warm()).await;
                tracing::debug!(
                    ms = started.elapsed().as_millis() as u64,
                    "Capital warm-up pass"
                );
                tokio::time::sleep(std::time::Duration::from_secs(WARMUP_EVERY_SECS)).await;
            }
        });
    }

    /// One warm-up pass (see [`Self::start_warmup`]). Errors are logged, not returned.
    pub fn warm(&self) {
        let Ok(accounts) = self.accounts() else {
            return;
        };
        for summary in accounts {
            let account = summary.account;
            if let Err(err) = self.scorecard(&account) {
                tracing::debug!(error = %err, "Capital warm-up: track record");
            }
            if let Err(err) = self.signal_board(&account) {
                tracing::debug!(error = %err, "Capital warm-up: signal board");
            }
            if let Err(err) = self.dividend_projection(
                &account,
                crate::dividends::MAX_YEARS,
                100.0,
                crate::dividends::Allocation::Proportional,
                false,
            ) {
                tracing::debug!(error = %err, "Capital warm-up: dividends");
            }
        }
    }

    pub fn start(self: &std::sync::Arc<Self>) {
        self.start_trade_schedule();
        self.start_warmup();
        if !self.sec_configured {
            tracing::info!("SEC refresh off: SAIL_SEC_USER_AGENT is not set");
            return;
        }
        let hours = std::env::var("SAIL_SEC_REFRESH_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|h| (1..=168).contains(h))
            .unwrap_or(DEFAULT_SEC_REFRESH_HOURS);
        let service = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            loop {
                let status = service.refresh_sec(None).await;
                tracing::info!(
                    issuers = status.issuers_checked,
                    new = status.new_evidence,
                    error = status.last_error.as_deref().unwrap_or(""),
                    "SEC refresh finished"
                );
                tokio::time::sleep(std::time::Duration::from_secs(hours * 3600)).await;
            }
        });
    }
}
