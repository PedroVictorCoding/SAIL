//! The EPIC's vertical slice, steps 2–7, end to end through the service. A fake SEC source
//! serves synthetic data (no network). Covered:
//! - an XTB holding links to its issuer;
//! - a filing arrives as evidence with all four times;
//! - it becomes a watch item or research proposal, never a trade;
//! - a replay is idempotent;
//! - a historical question before publication, or before SAIL saw it, can't see it;
//! - a vendor revision keeps the first observation time;
//! - SEC is never contacted unless configured, and a refusal stops the run.
use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use sail_capital::AsOf;
use sail_capital::evidence::*;
use sail_capital::service::CapitalService;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const CASH: &[u8] = include_bytes!("fixtures/xtb/english_cash.csv");
const CIK: u64 = 1_111_111;

struct FakeSec {
    calls: AtomicU32,
    description: Mutex<String>,
    refuse: bool,
}

impl FakeSec {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicU32::new(0),
            description: Mutex::new("Quarterly report".into()),
            refuse: false,
        })
    }
}

fn published(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 1, day, 21, 30, 0).unwrap()
}

fn filing(accession: &str, form: &str, items: &[&str], day: u32, desc: &str) -> Filing {
    Filing {
        accession: accession.into(),
        cik: Cik(CIK),
        form: form.into(),
        filing_date: NaiveDate::from_ymd_opt(2025, 1, day).unwrap(),
        published_at: published(day),
        published_basis: PublishedBasis::Acceptance,
        report_date: NaiveDate::from_ymd_opt(2024, 12, 28),
        items: items.iter().map(|s| s.to_string()).collect(),
        primary_document: Some("doc.htm".into()),
        description: Some(desc.into()),
        url: format!(
            "https://www.sec.gov/Archives/edgar/data/{CIK}/{}/doc.htm",
            accession.replace('-', "")
        ),
    }
}

#[async_trait]
impl EdgarApi for FakeSec {
    async fn ticker_index(&self) -> Result<TickerIndex, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(TickerIndex {
            fetched_at: Utc::now(),
            payload_sha256: "map-v1".into(),
            entries: vec![TickerEntry {
                cik: Cik(CIK),
                ticker: "AAPL".into(),
                title: "Example Fruit Computers Inc".into(),
            }],
        })
    }

    async fn issuer_filings(&self, cik: Cik) -> Result<IssuerFilings, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.refuse {
            return Err(SourceError::Refused(429));
        }
        let desc = self.description.lock().unwrap().clone();
        Ok(IssuerFilings {
            cik,
            name: "Example Fruit Computers Inc".into(),
            tickers: vec!["AAPL".into()],
            filings: vec![
                filing(
                    "0001111111-25-000002",
                    "8-K",
                    &["2.02", "9.01"],
                    30,
                    "Results",
                ),
                filing("0001111111-25-000001", "10-Q", &[], 20, &desc),
            ],
            payload_sha256: "sub-v1".into(),
        })
    }
}

fn service(dir: &std::path::Path, fake: Arc<FakeSec>, configured: bool) -> CapitalService {
    CapitalService::with_edgar(dir, fake, configured).unwrap()
}

fn now_bounds() -> AsOf {
    AsOf::now()
}

#[tokio::test]
async fn filing_to_holding_with_replay_and_point_in_time() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSec::new();
    let svc = service(dir.path(), fake.clone(), true);
    let start = Utc::now() - Duration::seconds(1);
    let import = svc
        .import_xtb(
            None,
            "cash.csv",
            CASH,
            None,
            None,
            start - Duration::days(1),
        )
        .unwrap();
    let account = import.account;

    // Step 2/3: the filing arrives as evidence.
    let status = svc.refresh_sec(None).await;
    assert_eq!(status.last_error, None, "{status:?}");
    assert_eq!(
        (
            status.issuers_checked,
            status.filings_seen,
            status.new_evidence
        ),
        (1, 2, 2)
    );

    // Step 5: it touches the AAPL.US holding, as research proposals (8-K 2.02, 10-Q).
    let view = svc.filings(&account, now_bounds()).unwrap();
    assert!(matches!(
        view.matches[0].matched,
        IssuerMatch::Matched { cik: Cik(CIK), .. }
    ));
    assert_eq!(view.items.len(), 2);
    let newest = &view.items[0];
    assert_eq!(newest.filing.form, "8-K");
    assert_eq!(newest.action, ResearchAction::ResearchProposal);
    assert_eq!(newest.published_at, published(30));
    assert!(newest.first_observed_at >= start);
    assert_eq!(newest.exposure.quantity.to_string(), "5");

    // Step 7a: replaying the same fetch adds nothing.
    let again = svc.refresh_sec(None).await;
    assert_eq!(again.new_evidence, 0);
    assert_eq!(svc.filings(&account, now_bounds()).unwrap().items.len(), 2);

    // Step 7b: asked as of before the 8-K was published, only the 10-Q is visible.
    let before_publication = AsOf {
        occurred_by: published(25),
        known_by: Utc::now(),
    };
    let items = svc.filings(&account, before_publication).unwrap().items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].filing.form, "10-Q");

    // Step 7c: asked as of before SAIL first saw anything, nothing is visible, and the
    // ticker map SAIL had then didn't exist either.
    let before_observation = AsOf {
        occurred_by: Utc::now(),
        known_by: start - Duration::seconds(5),
    };
    let view = svc.filings(&account, before_observation).unwrap();
    assert!(view.items.is_empty());
    assert!(matches!(
        view.matches[0].matched,
        IssuerMatch::Unresolved { .. }
    ));

    // A vendor revision: same accession, new content, first observation kept.
    let first_seen = newest_first_seen(&svc, &account, "10-Q");
    *fake.description.lock().unwrap() = "Quarterly report (corrected)".into();
    let revised = svc.refresh_sec(None).await;
    assert_eq!(revised.new_evidence, 1);
    let items = svc.filings(&account, now_bounds()).unwrap().items;
    let q = items.iter().find(|i| i.filing.form == "10-Q").unwrap();
    assert_eq!(
        q.filing.description.as_deref(),
        Some("Quarterly report (corrected)")
    );
    assert_eq!(q.first_observed_at, first_seen);
    assert_eq!(items.len(), 2, "a revision doesn't duplicate the item");
}

fn newest_first_seen(
    svc: &CapitalService,
    account: &sail_capital::AccountId,
    form: &str,
) -> DateTime<Utc> {
    svc.filings(account, now_bounds())
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.filing.form == form)
        .unwrap()
        .first_observed_at
}

#[tokio::test]
async fn unconfigured_sec_is_never_contacted() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSec::new();
    let svc = service(dir.path(), fake.clone(), false);
    svc.import_xtb(None, "cash.csv", CASH, None, None, Utc::now())
        .unwrap();
    let status = svc.refresh_sec(None).await;
    assert!(!status.configured);
    assert!(status.last_error.unwrap().contains("SAIL_SEC_USER_AGENT"));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_refusal_stops_the_run_and_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeSec {
        calls: AtomicU32::new(0),
        description: Mutex::new(String::new()),
        refuse: true,
    });
    let svc = service(dir.path(), fake.clone(), true);
    svc.import_xtb(None, "cash.csv", CASH, None, None, Utc::now())
        .unwrap();
    let status = svc.refresh_sec(None).await;
    assert!(status.last_error.unwrap().contains("429"));
    assert_eq!(status.new_evidence, 0);
    // One ticker-map call, one refused issuer call, nothing after.
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
}
