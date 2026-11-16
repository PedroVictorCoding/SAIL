//! Round 25: SEC Form 4s for the signal universe's US stocks, and the insider-buying
//! values they give ([`crate::signals::insider`]).

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, Utc};

use super::{CapitalService, ServiceError};
use crate::contracts::{AccountId, DecisionTime};
use crate::evidence::{Cik, SourceError};

/// Most Form 4 documents fetched in one SEC refresh.
pub const MAX_FORM4_PER_REFRESH: usize = 40;

/// Most watchlist issuers whose filing lists are fetched in one SEC refresh (held
/// issuers are already fetched by the evidence refresh).
pub const MAX_WATCH_ISSUERS_PER_REFRESH: usize = 20;

/// Form 4s older than this are never fetched (the signal's window is two weeks; two
/// years gives the walk-forward enough history).
const FORM4_LOOKBACK_DAYS: i64 = 730;

/// Stored filing lists younger than this are reused.
const FILINGS_MAX_AGE_DAYS: i64 = 7;

impl CapitalService {
    /// `(instrument key, CIK)` for the signal universe's US stocks with a unique SEC
    /// ticker match.
    pub(super) fn insider_issuers(
        &self,
        account: &AccountId,
        decision: DecisionTime,
    ) -> Result<Vec<(String, Cik)>, ServiceError> {
        let (universe, _) = self.signal_universe_as_of(account, decision)?;
        let mut out = Vec::new();
        for meta in universe.iter().filter(|m| m.class == "stock") {
            if let Some(cik) = self.cik_for(&meta.symbol, decision.at)? {
                out.push((meta.key.clone(), cik));
            }
        }
        Ok(out)
    }

    /// Fetches filing lists for watchlist issuers that have none fresh, then the Form 4
    /// documents not stored yet, newest first, within the caps. Returns how many Form 4s
    /// were stored. A refusal stops the run.
    pub(super) async fn refresh_insider_filings(&self) -> Result<u32, String> {
        let now = Utc::now();
        let accounts = self.accounts().map_err(|e| e.to_string())?;
        let mut ciks: Vec<Cik> = Vec::new();
        for summary in &accounts {
            for (_, cik) in self
                .insider_issuers(&summary.account, DecisionTime::now())
                .map_err(|e| e.to_string())?
            {
                if !ciks.contains(&cik) {
                    ciks.push(cik);
                }
            }
        }

        // Filing lists for issuers the evidence refresh doesn't cover.
        let mut listed = 0;
        for cik in &ciks {
            if listed >= MAX_WATCH_ISSUERS_PER_REFRESH {
                break;
            }
            let records = self
                .store
                .evidence_for(&[*cik])
                .map_err(|e| e.to_string())?;
            let fresh = records
                .iter()
                .map(|r| r.ingested_at)
                .max()
                .is_some_and(|at| now - at < Duration::days(FILINGS_MAX_AGE_DAYS));
            if fresh {
                continue;
            }
            listed += 1;
            match self.edgar.issuer_filings(*cik).await {
                Ok(filings) => {
                    self.store
                        .record_filings(&filings, Utc::now())
                        .map_err(|e| e.to_string())?;
                }
                Err(SourceError::Refused(code)) => {
                    return Err(SourceError::Refused(code).to_string());
                }
                Err(e) => tracing::warn!("filings for CIK {cik} not fetched: {e}"),
            }
        }

        // The Form 4 documents.
        let done = self.store.form4_accessions().map_err(|e| e.to_string())?;
        let records = self.store.evidence_for(&ciks).map_err(|e| e.to_string())?;
        let mut wanted: Vec<crate::evidence::Filing> = Vec::new();
        for record in records {
            let filing = record.filing;
            if matches!(filing.form.as_str(), "4" | "4/A")
                && now - filing.published_at < Duration::days(FORM4_LOOKBACK_DAYS)
                && !done.contains(&filing.accession)
                && !wanted.iter().any(|w| w.accession == filing.accession)
            {
                wanted.push(filing);
            }
        }
        wanted.sort_by_key(|f| std::cmp::Reverse(f.published_at));
        let mut stored = 0;
        for filing in wanted.into_iter().take(MAX_FORM4_PER_REFRESH) {
            let parsed = match self.edgar.form4(&filing).await {
                Ok(form4) => Ok(form4),
                Err(SourceError::Refused(code)) => {
                    return Err(SourceError::Refused(code).to_string());
                }
                Err(SourceError::NotConfigured) => break,
                Err(SourceError::Network(e)) => {
                    tracing::warn!("Form 4 {} not fetched: {e}", filing.accession);
                    continue;
                }
                Err(e) => Err(e.to_string()),
            };
            let added = self
                .store
                .record_form4(&filing, parsed.as_ref().map_err(String::as_str), Utc::now())
                .map_err(|e| e.to_string())?;
            if added && parsed.is_ok() {
                stored += 1;
            }
        }
        Ok(stored)
    }

    /// Insider-buying values for `keys` (instrument key → CIK) on their `sessions`, from
    /// the Form 4s fetched by `decision`.
    pub(super) fn insider_scores_as_of(
        &self,
        keys: &[(String, Cik)],
        sessions: &BTreeMap<String, Vec<NaiveDate>>,
        decision: DecisionTime,
    ) -> Result<Option<crate::signals::ExternalScores>, ServiceError> {
        let mut filings = BTreeMap::new();
        for (key, cik) in keys {
            let stored = self.store.form4s(*cik, decision.at)?;
            if !stored.is_empty() {
                filings.insert(key.clone(), stored);
            }
        }
        if filings.is_empty() {
            return Ok(None);
        }
        let values = crate::signals::insider::cluster_scores(&filings, sessions);
        Ok(
            (!values.is_empty()).then_some(crate::signals::ExternalScores {
                signal: crate::signals::SignalId::InsiderBuying,
                values,
            }),
        )
    }

    /// Today's insider clusters for `account`: `(symbol, distinct buyers)`.
    pub fn insider_clusters(
        &self,
        account: &AccountId,
    ) -> Result<Vec<(String, usize)>, ServiceError> {
        let decision = DecisionTime::now();
        let issuers = self.insider_issuers(account, decision)?;
        let mut filings = BTreeMap::new();
        let mut sessions = BTreeMap::new();
        let mut symbols = BTreeMap::new();
        let (universe, _) = self.signal_universe_as_of(account, decision)?;
        for (key, cik) in &issuers {
            let stored = self.store.form4s(*cik, decision.at)?;
            if stored.is_empty() {
                continue;
            }
            let days: Vec<NaiveDate> = self
                .store
                .prices(key, decision.at)?
                .iter()
                .map(|o| o.session)
                .collect();
            sessions.insert(key.clone(), days);
            filings.insert(key.clone(), stored);
            if let Some(meta) = universe.iter().find(|m| &m.key == key) {
                symbols.insert(key.clone(), meta.symbol.clone());
            }
        }
        Ok(
            crate::signals::insider::current_clusters(&filings, &sessions)
                .into_iter()
                .map(|(key, n)| (symbols.get(&key).cloned().unwrap_or(key), n))
                .collect(),
        )
    }
}

/// One instrument's `(key, symbol, closes)`, as the price loaders return it.
type CloseRow = (String, String, Vec<(NaiveDate, rust_decimal::Decimal)>);

/// Sessions per key from `(key, symbol, closes)` rows.
pub(super) fn sessions_of(data: &[CloseRow]) -> BTreeMap<String, Vec<NaiveDate>> {
    data.iter()
        .map(|(key, _, closes)| (key.clone(), closes.iter().map(|(d, _)| *d).collect()))
        .collect()
}
