//! Live web research for the early-retirement plan (Capital → Income → Dividend projection).
//!
//! A run fetches a vetted set of public pages, hands their readable text and the owner's
//! deterministic plan to SAIL's local model, and validates the answer before storing it:
//! citations must point at fetched sources and every list is capped. The latest brief is
//! kept on disk so a restart does not lose it.
//!
//! Nothing here contacts a broker or sends portfolio holdings anywhere: only the eight
//! public URLs below are requested, and inference stays on this host (unless the owner has
//! deliberately paired a Mesh worker; Mind decides that, not this module).
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::StreamExt;
use sail_capital::dividends::{
    DividendOutlook,
    retirement::Plan,
    retirement_research::{
        self, Brief, BriefSource, MAX_EXCERPT_CHARS, MAX_SOURCES, SourceExcerpt, truncate,
    },
};
use sail_protocol::{Generation, InferenceRequest, Message, Role, resources::WorkPriority};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::html_text;

/// File under the Capital data directory that keeps the latest brief.
pub const RESEARCH_FILE: &str = "retirement-research.json";

/// Most tokens the model may write for one brief. Five strategies with their actions,
/// tradeoffs and citations, plus the lists, need about 1,600; 1,100 cut answers off
/// mid-JSON.
const BRIEF_MAX_TOKENS: u32 = 2_400;

/// Note shown when the answer was cut off and only its complete parts were kept.
const CUT_OFF_NOTE: &str =
    "The local model's answer was cut off before the end, so only its complete parts are shown.";

/// Note shown when the answer was not structured at all.
const UNSTRUCTURED_NOTE: &str = "The model's answer was not valid structured JSON, so this is its reply without checked citations.";

/// Re-reads a stored brief that was kept as raw text: an answer that was cut off mid-JSON
/// becomes the strategies it completed; raw JSON is never left on the page.
pub fn repair_stored(brief: &mut Brief) {
    if brief.structured {
        return;
    }
    let raw = brief.summary.clone();
    let (draft, structured) = retirement_research::draft_from_response(&raw, brief.sources.len());
    brief.headline = draft.headline;
    brief.summary = draft.summary;
    brief.strategies = draft.strategies;
    brief.risks = draft.risks;
    brief.next_steps = draft.next_steps;
    brief.assumptions = draft.assumptions;
    if structured {
        brief.structured = true;
        let cited: Vec<u32> = brief
            .strategies
            .iter()
            .flat_map(|strategy| strategy.citations.iter().copied())
            .collect();
        for source in &mut brief.sources {
            source.cited = cited.contains(&source.number);
        }
        brief.notes.retain(|note| note != UNSTRUCTURED_NOTE);
        if !brief.notes.iter().any(|note| note == CUT_OFF_NOTE) {
            brief.notes.insert(0, CUT_OFF_NOTE.to_string());
        }
    }
}

/// The latest stored brief for the voice assistant, from `dir`: the status, when it was
/// written, the headline, the strategy names and `read_aloud`, the whole brief as plain
/// sentences for speech. `None` when no brief was ever written.
pub fn brief_for_voice(dir: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(dir.join(RESEARCH_FILE)).ok()?;
    let snapshot: ResearchSnapshot = serde_json::from_slice(&bytes).ok()?;
    let mut brief = snapshot.brief?;
    repair_stored(&mut brief);
    let draft = retirement_research::Draft {
        headline: brief.headline.clone(),
        summary: brief.summary.clone(),
        strategies: brief.strategies.clone(),
        risks: brief.risks.clone(),
        next_steps: brief.next_steps.clone(),
        assumptions: brief.assumptions.clone(),
    };
    Some(serde_json::json!({
        "generated_at": brief.generated_at,
        "headline": brief.headline,
        "strategies": brief.strategies.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        "next_steps": brief.next_steps,
        "read_aloud": retirement_research::speakable(&draft),
        "note": "Planning information from SAIL's local model and fetched public pages, not financial advice. Read `read_aloud` to the owner as it is.",
    }))
}

/// Domains a run may fetch. A page is never fetched from anywhere else.
const RESEARCH_HOSTS: &[&str] = &[
    "wikipedia.org",
    "earlyretirementnow.com",
    "europa.eu",
    "portaldasfinancas.gov.pt",
    "ecb.europa.eu",
    "bogleheads.org",
];

/// The vetted pages fetched on every run, live.
const SEEDS: &[(&str, &str)] = &[
    (
        "Safe withdrawal rate (Wikipedia)",
        "https://en.wikipedia.org/wiki/Safe_withdrawal_rate",
    ),
    (
        "Trinity study (Wikipedia)",
        "https://en.wikipedia.org/wiki/Trinity_study",
    ),
    (
        "FIRE movement (Wikipedia)",
        "https://en.wikipedia.org/wiki/FIRE_movement",
    ),
    (
        "Dividend investing (Wikipedia)",
        "https://en.wikipedia.org/wiki/Dividend_investing",
    ),
    (
        "William Bengen, the 4% rule (Wikipedia)",
        "https://en.wikipedia.org/wiki/William_Bengen",
    ),
    (
        "Safe Withdrawal Rate series (Early Retirement Now)",
        "https://earlyretirementnow.com/safe-withdrawal-rate-series/",
    ),
    (
        "Portuguese IRS Code, article 72 (Autoridade Tributária)",
        "https://info.portaldasfinancas.gov.pt/pt/informacao_fiscal/codigos_tributarios/cirs_rep/Pages/irs72.aspx",
    ),
    (
        "Double taxation in the EU (European Commission)",
        "https://europa.eu/youreurope/citizens/work/taxes/double-taxation/index_en.htm",
    ),
];

/// How far ahead a run fetches.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest page accepted, before text extraction.
const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
/// Smallest readable text a page must yield to be offered to the model.
const MIN_PAGE_CHARS: usize = 400;
/// How long the local model has to write the brief.
const MODEL_TIMEOUT: Duration = Duration::from_secs(600);

/// Where a research run stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchStatus {
    /// No run yet, or the last one was cleared.
    #[default]
    Idle,
    /// Fetching sources or waiting for the model.
    Running,
    /// The latest brief is available.
    Done,
    /// The last run failed; `last_error` says why.
    Failed,
}

/// One source the run tried.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRecord {
    /// Page title (the seed's label when the page did not provide one).
    pub title: String,
    /// URL requested.
    pub url: String,
    /// Whether it was fetched and yielded enough text.
    pub ok: bool,
    /// Owner-readable reason it was not used, when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Readable characters extracted (0 when the fetch failed).
    pub chars: usize,
}

/// What `GET /dividends/retirement/research` returns.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResearchSnapshot {
    /// Current state.
    pub status: ResearchStatus,
    /// When the run began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// When it finished, successfully or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// Why the last run failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Model that wrote the brief, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The latest brief, kept while a new run is in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<Brief>,
    /// Every source this run tried, with what happened.
    #[serde(default)]
    pub sources: Vec<SourceRecord>,
}

impl ResearchSnapshot {
    fn interrupted() -> Self {
        Self {
            status: ResearchStatus::Failed,
            last_error: Some(
                "The previous research run was interrupted before it finished.".into(),
            ),
            ..Self::default()
        }
    }
}

/// The research runner: one run at a time, latest brief persisted.
pub struct ResearchEngine {
    mind: sail_mind::MindService,
    client: reqwest::Client,
    path: PathBuf,
    state: Mutex<ResearchSnapshot>,
    running: AtomicBool,
}

impl ResearchEngine {
    /// Opens the engine, loading the last brief from `dir` when it is readable. A corrupt
    /// file is ignored (and logged) rather than blocking Capital.
    pub fn open(mind: sail_mind::MindService, dir: &Path) -> Self {
        let path = dir.join(RESEARCH_FILE);
        let state = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ResearchSnapshot>(&bytes) {
                Ok(snapshot) if snapshot.status == ResearchStatus::Running => {
                    ResearchSnapshot::interrupted()
                }
                Ok(mut snapshot) => {
                    if let Some(brief) = snapshot.brief.as_mut() {
                        repair_stored(brief);
                    }
                    snapshot
                }
                Err(error) => {
                    tracing::warn!(error = %error, path = %path.display(), "retirement research state is unreadable");
                    ResearchSnapshot::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ResearchSnapshot::default()
            }
            Err(error) => {
                tracing::warn!(error = %error, path = %path.display(), "retirement research state cannot be read");
                ResearchSnapshot::default()
            }
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(FETCH_TIMEOUT)
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/130.0.0.0 Safari/537.36 SAIL-Capital-Research",
            )
            .build();
        let client = match client {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(error = %error, "retirement research HTTP client unavailable");
                reqwest::Client::new()
            }
        };
        Self {
            mind,
            client,
            path,
            state: Mutex::new(state),
            running: AtomicBool::new(false),
        }
    }

    /// The stored snapshot.
    pub async fn snapshot(&self) -> ResearchSnapshot {
        self.state.lock().await.clone()
    }

    /// Starts a run unless one is in progress. The returned flag says whether this call
    /// was the one that started it. The snapshot reads `running` as soon as this returns.
    pub async fn start(self: &Arc<Self>, outlook: DividendOutlook, plan: Plan) -> bool {
        if self.running.swap(true, Ordering::SeqCst) {
            return false;
        }
        {
            let mut state = self.state.lock().await;
            state.status = ResearchStatus::Running;
            state.started_at = Some(Utc::now());
            state.finished_at = None;
            state.last_error = None;
            state.sources.clear();
            persist(&self.path, &state);
        }
        let engine = self.clone();
        tokio::spawn(async move {
            engine.run(outlook, plan).await;
            engine.running.store(false, Ordering::SeqCst);
        });
        true
    }

    async fn run(&self, outlook: DividendOutlook, plan: Plan) {
        match self.research(outlook, plan).await {
            Ok((brief, sources)) => {
                let mut state = self.state.lock().await;
                state.status = ResearchStatus::Done;
                state.finished_at = Some(Utc::now());
                state.last_error = None;
                state.model = Some(brief.model.clone());
                state.brief = Some(brief);
                state.sources = sources;
                persist(&self.path, &state);
            }
            Err(error) => {
                let mut state = self.state.lock().await;
                state.status = ResearchStatus::Failed;
                state.finished_at = Some(Utc::now());
                state.last_error = Some(error);
                persist(&self.path, &state);
            }
        }
    }

    async fn research(
        &self,
        outlook: DividendOutlook,
        plan: Plan,
    ) -> Result<(Brief, Vec<SourceRecord>), String> {
        let fetched_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let fetched =
            futures_util::future::join_all(SEEDS.iter().map(|(title, url)| self.fetch(title, url)))
                .await;
        let mut records = Vec::with_capacity(fetched.len());
        let mut excerpts = Vec::new();
        for (record, excerpt) in fetched {
            if let Some(excerpt) = excerpt {
                excerpts.push(excerpt);
            }
            records.push(record);
        }
        excerpts.truncate(MAX_SOURCES);
        if excerpts.is_empty() {
            return Err(
                "No research source could be fetched. Check this host's internet connection and try again."
                    .into(),
            );
        }
        let user = retirement_research::user_prompt(&plan, &outlook, &excerpts);
        let mut request = InferenceRequest::text(user.clone());
        request.messages = vec![
            Message::text(Role::System, retirement_research::system_prompt()),
            Message::text(Role::User, user),
        ];
        request.generation = Generation::direct(BRIEF_MAX_TOKENS, 0.25);
        let response = tokio::time::timeout(
            MODEL_TIMEOUT,
            self.mind.infer_with_priority(
                request,
                CancellationToken::new(),
                None,
                WorkPriority::Background,
            ),
        )
        .await
        .map_err(|_| "The local model did not finish the research brief in time.".to_string())?
        .map_err(|error| format!("The local model did not answer: {}", error.message))?;
        let raw = response
            .message
            .content
            .text()
            .unwrap_or_default()
            .to_string();
        if raw.trim().is_empty() {
            return Err("The local model returned an empty research brief.".into());
        }
        let (draft, structured) = retirement_research::draft_from_response(&raw, excerpts.len());
        let cut_off = structured && retirement_research::was_cut_off(&raw);
        let cited: Vec<u32> = draft
            .strategies
            .iter()
            .flat_map(|strategy| strategy.citations.iter().copied())
            .collect();
        let sources: Vec<BriefSource> = excerpts
            .iter()
            .enumerate()
            .map(|(index, excerpt)| {
                let number = index as u32 + 1;
                BriefSource {
                    number,
                    title: excerpt.title.clone(),
                    url: excerpt.url.clone(),
                    retrieved_at: fetched_at.clone(),
                    cited: cited.contains(&number),
                }
            })
            .collect();
        let mut notes = vec![
            format!(
                "Written by SAIL's local model from {} fetched source{}; [S1]… in the strategies refer to the list below.",
                sources.len(),
                if sources.len() == 1 { "" } else { "s" }
            ),
            "Pages change; each source shows when it was fetched. A language model can misread a page, so check a citation before acting on it.".into(),
            "This is planning information, not regulated financial advice.".into(),
        ];
        if !structured {
            notes.insert(0, UNSTRUCTURED_NOTE.into());
        }
        if cut_off {
            notes.insert(0, CUT_OFF_NOTE.into());
        }
        if sources.len() < 4 {
            notes.insert(
                0,
                "Fewer than four sources could be fetched; the brief rests on the ones listed."
                    .into(),
            );
        }
        Ok((
            Brief {
                headline: draft.headline,
                summary: draft.summary,
                strategies: draft.strategies,
                risks: draft.risks,
                next_steps: draft.next_steps,
                assumptions: draft.assumptions,
                sources,
                model: response.model,
                generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                structured,
                notes,
            },
            records,
        ))
    }

    /// Fetch one vetted page and extract readable text.
    async fn fetch(&self, label: &str, url: &str) -> (SourceRecord, Option<SourceExcerpt>) {
        let mut record = SourceRecord {
            title: label.to_string(),
            url: url.to_string(),
            ok: false,
            error: None,
            chars: 0,
        };
        match self.fetch_text(url).await {
            Ok((title, text)) => {
                record.ok = true;
                record.title = title.unwrap_or_else(|| label.to_string());
                record.chars = text.chars().count();
                let excerpt = SourceExcerpt {
                    title: record.title.clone(),
                    url: url.to_string(),
                    text: truncate(&text, MAX_EXCERPT_CHARS),
                };
                (record, Some(excerpt))
            }
            Err(error) => {
                record.error = Some(error);
                (record, None)
            }
        }
    }

    async fn fetch_text(&self, raw: &str) -> Result<(Option<String>, String), String> {
        let url = reqwest::Url::parse(raw).map_err(|_| "invalid URL".to_string())?;
        if url.scheme() != "https" {
            return Err("only https pages are fetched".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("URLs with credentials are refused".into());
        }
        let host = url.host_str().ok_or_else(|| "no host".to_string())?;
        if !allowed_host(host) {
            return Err("the domain is not on the research list".into());
        }
        let host = host.to_string();
        let resolved = tokio::net::lookup_host((host.as_str(), 443))
            .await
            .map_err(|_| "the page host did not resolve".to_string())?;
        if resolved.into_iter().any(|socket| private_ip(socket.ip())) {
            return Err("the page resolves to a private address".into());
        }
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| "the page could not be reached".to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status().as_u16()));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "the page stopped sending".to_string())?;
            if body.len() + chunk.len() > MAX_PAGE_BYTES {
                return Err("the page is too large".into());
            }
            body.extend_from_slice(&chunk);
        }
        let html = String::from_utf8_lossy(&body);
        let title = html_text::title(&html);
        let readable = html_text::readable(&html, 8);
        if readable.chars().count() < MIN_PAGE_CHARS {
            return Err("the page had too little readable text".into());
        }
        Ok((title, readable))
    }
}

fn allowed_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    RESEARCH_HOSTS
        .iter()
        .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
}

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.is_unspecified()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn persist(path: &Path, snapshot: &ResearchSnapshot) {
    let Ok(bytes) = serde_json::to_vec_pretty(snapshot) else {
        return;
    };
    let temporary = path.with_extension("tmp");
    if let Err(error) = std::fs::write(&temporary, &bytes) {
        tracing::warn!(error = %error, path = %path.display(), "retirement research state cannot be saved");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }
    // Windows `rename` refuses to replace an existing file; the state is a single writer's
    // snapshot, so replacing it is what "save the latest" means there too.
    #[cfg(windows)]
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(error = %error, path = %path.display(), "old retirement research state cannot be replaced");
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        tracing::warn!(error = %error, path = %path.display(), "retirement research state cannot be saved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A brief stored before the fix: the model's answer cut off mid-JSON, kept as raw text.
    fn raw_stored_brief() -> Brief {
        Brief {
            headline: "Research note".into(),
            summary: r#"{ "headline": "Early Retirement Feasibility", "summary": "There is a gap [S1].",
              "strategies": [ { "name": "Increase Monthly Contribution", "why": "The most direct lever [S3].",
                "actions": ["Raise the allowance"], "tradeoffs": ["Less to spend"], "citations": [3] },
              { "name": "Optimize Tax Efficiency", "why": "Holding international ETFs (XMPL.DE"#
                .into(),
            strategies: Vec::new(),
            risks: Vec::new(),
            next_steps: Vec::new(),
            assumptions: Vec::new(),
            sources: (1..=4)
                .map(|number| BriefSource {
                    number,
                    title: format!("Source {number}"),
                    url: format!("https://en.wikipedia.org/wiki/S{number}"),
                    retrieved_at: "2026-11-15T17:52:00Z".into(),
                    cited: false,
                })
                .collect(),
            model: "local".into(),
            generated_at: "2026-11-15T17:53:00Z".into(),
            structured: false,
            notes: vec![UNSTRUCTURED_NOTE.into()],
        }
    }

    #[test]
    fn a_stored_cut_off_brief_is_repaired_on_load() {
        let mut brief = raw_stored_brief();
        repair_stored(&mut brief);
        assert!(brief.structured);
        assert_eq!(brief.headline, "Early Retirement Feasibility");
        assert_eq!(brief.strategies.len(), 1);
        assert!(brief.sources[2].cited && !brief.sources[0].cited);
        assert_eq!(brief.notes, vec![CUT_OFF_NOTE.to_string()]);
        // Idempotent.
        let again = brief.clone();
        repair_stored(&mut brief);
        assert_eq!(brief, again);
    }

    #[test]
    fn the_voice_tool_gets_the_brief_as_plain_sentences() {
        let dir = tempfile::tempdir().unwrap();
        assert!(brief_for_voice(dir.path()).is_none());
        let snapshot = ResearchSnapshot {
            status: ResearchStatus::Done,
            brief: Some(raw_stored_brief()),
            ..ResearchSnapshot::default()
        };
        std::fs::write(
            dir.path().join(RESEARCH_FILE),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        let voice = brief_for_voice(dir.path()).unwrap();
        let spoken = voice["read_aloud"].as_str().unwrap();
        assert!(spoken.starts_with("Early Retirement Feasibility. There is a gap."));
        assert!(spoken.contains("Strategy 1: Increase Monthly Contribution."));
        assert!(!spoken.contains('{') && !spoken.contains("[S"));
        assert_eq!(voice["strategies"][0], "Increase Monthly Contribution");
    }

    #[test]
    fn only_listed_domains_are_fetchable() {
        assert!(allowed_host("en.wikipedia.org"));
        assert!(allowed_host("wikipedia.org"));
        assert!(allowed_host("info.portaldasfinancas.gov.pt"));
        assert!(allowed_host("europa.eu"));
        assert!(!allowed_host("wikipedia.org.evil.example"));
        assert!(!allowed_host("evilwikipedia.org"));
        assert!(!allowed_host("example.com"));
    }

    #[test]
    fn private_addresses_are_refused() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(private_ip(ip.parse().expect("IP")), "{ip}");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(!private_ip(ip.parse().expect("IP")), "{ip}");
        }
    }

    #[test]
    fn the_latest_snapshot_round_trips_through_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(RESEARCH_FILE);
        let snapshot = ResearchSnapshot {
            status: ResearchStatus::Done,
            last_error: Some("fixture".into()),
            model: Some("fixture-model".into()),
            ..ResearchSnapshot::default()
        };
        persist(&path, &snapshot);
        let loaded: ResearchSnapshot =
            serde_json::from_slice(&std::fs::read(&path).expect("read back")).expect("parse");
        assert_eq!(loaded.status, ResearchStatus::Done);
        assert_eq!(loaded.last_error.as_deref(), Some("fixture"));
        assert_eq!(loaded.model.as_deref(), Some("fixture-model"));
        persist(&path, &ResearchSnapshot::interrupted());
        let loaded: ResearchSnapshot =
            serde_json::from_slice(&std::fs::read(&path).expect("read back")).expect("parse");
        assert_eq!(loaded.status, ResearchStatus::Failed);
    }

    #[test]
    fn a_running_state_is_reported_as_interrupted() {
        let snapshot = ResearchSnapshot::interrupted();
        assert_eq!(snapshot.status, ResearchStatus::Failed);
        assert!(snapshot.last_error.unwrap().contains("interrupted"));
    }
}
