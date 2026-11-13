//! Rust-owned stock forecast jobs. Only the TimesFM tensor runtime is a Python worker.
mod chart;
mod yahoo;
use chrono::{DateTime, Datelike, Duration, Months, Utc};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MODEL: &str = "google/timesfm-2.5-200m-pytorch";
pub const PERIODS: &[&str] = &[
    "1d", "5d", "1mo", "3mo", "6mo", "1y", "2y", "5y", "10y", "ytd", "max",
];
pub const INTERVALS: &[&str] = &[
    "1m", "2m", "5m", "15m", "30m", "1h", "90m", "1d", "5d", "1wk", "1mo", "3mo",
];
pub const MAX_CONTEXT: usize = 15_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub symbol: String,
    pub period: String,
    pub interval: String,
    pub horizon: usize,
}
impl Request {
    pub fn validate(&mut self) -> Result<(), String> {
        self.symbol = self.symbol.trim().to_ascii_uppercase();
        if self.symbol.is_empty()
            || self.symbol.len() > 32
            || !self
                .symbol
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-^=".contains(&b))
        {
            return Err("Enter a Yahoo Finance ticker (for example AAPL or EDP.LS).".into());
        }
        if !PERIODS.contains(&self.period.as_str())
            || !INTERVALS.contains(&self.interval.as_str())
            || !(1..=1000).contains(&self.horizon)
        {
            return Err("Choose a supported period, interval and horizon of 1–1000 steps.".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub time: DateTime<Utc>,
    pub close: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForecastStep {
    pub step: usize,
    /// Calendar projection only: not a promised exchange session.
    pub calendar_time: DateTime<Utc>,
    pub median: f64,
    /// Deciles 0.1..0.9, model estimates, not empirically calibrated intervals.
    pub deciles: [f64; 9],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultView {
    pub chart_svg: String,
    pub symbol: String,
    pub currency: Option<String>,
    pub exchange_timezone: String,
    pub source: String,
    pub fetched_at: DateTime<Utc>,
    pub adjustment: String,
    pub model: String,
    pub model_revision: String,
    pub context_points: usize,
    pub history: Vec<Observation>,
    pub forecast: Vec<ForecastStep>,
    pub notes: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Completed,
    Failed,
    Interrupted,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub request: Request,
    pub state: JobState,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub result: Option<ResultView>,
}
#[derive(Serialize)]
pub struct Status {
    pub enabled: bool,
    pub yahoo_enabled: bool,
    pub model: &'static str,
    pub job: Option<Job>,
}
/// One active, persisted job per Capital host. Disconnecting the browser does not cancel it.
pub struct Engine {
    dir: PathBuf,
    job: Mutex<Option<Job>>,
    running: Arc<tokio::sync::Semaphore>,
}
impl Engine {
    pub fn new(dir: PathBuf) -> Self {
        let mut job = std::fs::read(dir.join("latest.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Job>(&b).ok());
        if let Some(j) = &mut job {
            if j.state == JobState::Running {
                j.state = JobState::Interrupted;
                j.error =
                    Some("SAIL restarted before this forecast finished. Run it again.".into());
                j.finished_at = Some(Utc::now());
            }
        }
        Self {
            dir,
            job: Mutex::new(job),
            running: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
    pub fn status(&self) -> Status {
        Status {
            enabled: std::env::var("SAIL_TIMESFM_ENABLED").as_deref() == Ok("1"),
            yahoo_enabled: matches!(
                std::env::var("SAIL_PRICES_SOURCE").as_deref(),
                Ok("yfinance" | "yahoo")
            ),
            model: MODEL,
            job: self.job.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    }
    fn save(&self, job: Job) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|_| "Cannot create the local forecast directory.".to_string())?;
        let bytes = serde_json::to_vec(&job).map_err(|_| "Cannot encode forecast.".to_string())?;
        let tmp = self.dir.join("latest.tmp");
        std::fs::write(&tmp, bytes)
            .and_then(|_| std::fs::rename(tmp, self.dir.join("latest.json")))
            .map_err(|_| "Cannot save the local forecast job.".to_string())?;
        *self.job.lock().unwrap_or_else(|e| e.into_inner()) = Some(job);
        Ok(())
    }
    pub fn start(self: &Arc<Self>, mut request: Request) -> Result<Job, String> {
        request.validate()?;
        let status = self.status();
        if !status.enabled {
            return Err("TimesFM is disabled. Follow docs/CAPITAL_STOCK_FORECAST.md to install and enable it.".into());
        }
        if !status.yahoo_enabled {
            return Err("Yahoo access is disabled. Set SAIL_PRICES_SOURCE=yfinance to opt in to sending ticker queries to Yahoo.".into());
        }
        let permit = self
            .running
            .clone()
            .try_acquire_owned()
            .map_err(|_| "A stock forecast is already running.".to_string())?;
        let now = Utc::now();
        let job = Job {
            id: now.timestamp_nanos_opt().unwrap_or_default().to_string(),
            request,
            state: JobState::Running,
            started_at: now,
            finished_at: None,
            error: None,
            result: None,
        };
        self.save(job.clone())?;
        let engine = self.clone();
        let mut work = job.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result =
                tokio::time::timeout(StdDuration::from_secs(300), run(&work.request)).await;
            match result {
                Ok(Ok(view)) => {
                    work.state = JobState::Completed;
                    work.result = Some(view);
                }
                other => {
                    work.state = JobState::Failed;
                    work.error = Some(match other {
                        Ok(Err(e)) => e,
                        _ => "Forecast timed out after five minutes.".into(),
                    });
                }
            }
            work.finished_at = Some(Utc::now());
            if let Err(e) = engine.save(work.clone()) {
                work.state = JobState::Failed;
                work.result = None;
                work.error = Some(e);
                *engine.job.lock().unwrap_or_else(|e| e.into_inner()) = Some(work);
            }
        });
        Ok(job)
    }
}

pub fn period_start(end: DateTime<Utc>, period: &str) -> Option<DateTime<Utc>> {
    match period {
        "max" => None,
        "ytd" => end
            .with_month(1)?
            .with_day(1)
            .and_then(|d| d.date_naive().and_hms_opt(0, 0, 0).map(|t| t.and_utc())),
        "1d" => end.checked_sub_signed(Duration::days(1)),
        "5d" => end.checked_sub_signed(Duration::days(5)),
        _ => {
            let months = match period {
                "1mo" => 1,
                "3mo" => 3,
                "6mo" => 6,
                "1y" => 12,
                "2y" => 24,
                "5y" => 60,
                "10y" => 120,
                _ => return None,
            };
            end.checked_sub_months(Months::new(months))
        }
    }
}
pub fn calendar_time(end: DateTime<Utc>, interval: &str, step: usize) -> Option<DateTime<Utc>> {
    let step = u32::try_from(step).ok()?;
    let seconds = match interval {
        "1m" => 60,
        "2m" => 120,
        "5m" => 300,
        "15m" => 900,
        "30m" => 1800,
        "1h" => 3600,
        "90m" => 5400,
        "1d" => 86400,
        "5d" => 432000,
        "1wk" => 604800,
        "1mo" => return end.checked_add_months(Months::new(step)),
        "3mo" => return end.checked_add_months(Months::new(step.checked_mul(3)?)),
        _ => return None,
    };
    end.checked_add_signed(Duration::seconds(seconds * i64::from(step)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerOutput {
    model: String,
    revision: String,
    median: Vec<f64>,
    deciles: Vec<[f64; 9]>,
}
fn parse_output(bytes: &[u8], horizon: usize) -> Result<WorkerOutput, String> {
    let out: WorkerOutput = serde_json::from_slice(bytes).map_err(|_| {
        "TimesFM returned an invalid response; check the local worker installation.".to_string()
    })?;
    if out.model != MODEL
        || out.revision.len() != 40
        || !out.revision.bytes().all(|b| b.is_ascii_hexdigit())
        || out.median.len() != horizon
        || out.deciles.len() != horizon
        || out.median.iter().any(|v| !v.is_finite() || *v < 0.)
        || out.deciles.iter().any(|q| {
            q.iter().any(|v| !v.is_finite() || *v < 0.) || q.windows(2).any(|w| w[0] > w[1])
        })
        || out
            .median
            .iter()
            .zip(&out.deciles)
            .any(|(m, q)| (m - q[4]).abs() > 1e-5 * m.abs().max(1.))
    {
        return Err("TimesFM output failed model identity, shape or quantile checks.".into());
    }
    Ok(out)
}
async fn worker(closes: Vec<f64>, horizon: usize) -> Result<WorkerOutput, String> {
    let python = std::env::var("SAIL_TIMESFM_PYTHON")
        .unwrap_or_else(|_| "/srv/sail-data/venvs/timesfm/bin/python".into());
    let script = std::env::var("SAIL_TIMESFM_SCRIPT")
        .unwrap_or_else(|_| "scripts/prediction/timesfm_worker.py".into());
    let checkpoint = std::env::var("SAIL_TIMESFM_CHECKPOINT")
        .unwrap_or_else(|_| "/srv/sail-data/models/timesfm-2.5".into());
    let mut child = tokio::process::Command::new(python)
        .arg("-I")
        .arg(script)
        .arg(checkpoint)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HF_HUB_OFFLINE", "1")
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("OMP_NUM_THREADS", "2")
        .env("MKL_NUM_THREADS", "2")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            "TimesFM worker is not installed. Follow docs/CAPITAL_STOCK_FORECAST.md.".to_string()
        })?;
    let mut stdin = child.stdin.take().ok_or("Cannot open worker input.")?;
    let mut stdout = child.stdout.take().ok_or("Cannot open worker output.")?;
    let input = serde_json::to_vec(&serde_json::json!({"closes":closes,"horizon":horizon}))
        .map_err(|_| "Cannot encode worker input.")?;
    let write = async move {
        stdin.write_all(&input).await?;
        stdin.shutdown().await
    };
    let read = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    };
    let (write, read) = tokio::join!(write, read);
    write.map_err(|_| {
        "TimesFM could not read the price series; verify the worker setup.".to_string()
    })?;
    let bytes = read.map_err(|_| "Cannot read TimesFM output.".to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("TimesFM output exceeded its size limit.".into());
    }
    let status = child
        .wait()
        .await
        .map_err(|_| "Cannot wait for TimesFM.".to_string())?;
    if !status.success() {
        return Err("TimesFM inference failed. Verify its pinned runtime and local checkpoint using the setup guide.".into());
    }
    parse_output(&bytes, horizon)
}
async fn run(request: &Request) -> Result<ResultView, String> {
    let data = yahoo::fetch(request).await?;
    let fetched_at = Utc::now();
    let last = data
        .bars
        .last()
        .ok_or("Yahoo returned no completed positive adjusted closes.")?
        .time;
    let context = &data.bars[data.bars.len().saturating_sub(MAX_CONTEXT)..];
    if context.len() < 32 {
        return Err("TimesFM needs at least 32 completed price bars. Choose a coarser interval or another ticker.".into());
    }
    let output = worker(context.iter().map(|b| b.close).collect(), request.horizon).await?;
    let forecast = output
        .median
        .into_iter()
        .zip(output.deciles)
        .enumerate()
        .map(|(i, (median, deciles))| {
            Ok(ForecastStep {
                step: i + 1,
                calendar_time: calendar_time(last, &request.interval, i + 1)
                    .ok_or("Forecast dates exceed the supported calendar.")?,
                median,
                deciles,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let context_points = context.len();
    let start = period_start(last, &request.period);
    let history: Vec<Observation> = data
        .bars
        .into_iter()
        .filter(|b| start.is_none_or(|s| b.time >= s))
        .collect();
    let chart_svg = chart::svg(&history, &forecast);
    Ok(ResultView { chart_svg, symbol: request.symbol.clone(), currency: data.currency, exchange_timezone: data.timezone, source: "Yahoo Finance chart API".into(), fetched_at, adjustment: data.adjustment, model: output.model, model_revision: output.revision, context_points, history, forecast, notes: vec![
        "Experimental zero-shot forecast; no demonstrated stock-prediction accuracy or calibrated coverage. Not an order or a trading recommendation.".into(),
        "Forecast steps are observation intervals. Calendar projections include market closures; they are not exchange-session timestamps.".into(),
        "Display period filters the chart only. The model uses up to 15,000 latest completed bars from the provider's available history.".into(),
        data.note,
    ] })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn date(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    #[tokio::test]
    #[ignore = "requires the locally installed TimesFM checkpoint and CPU runtime"]
    async fn installed_timesfm_worker_smoke() {
        let closes = (0..64).map(|i| 100. + i as f64 / 10.).collect();
        let out = worker(closes, 5).await.unwrap();
        assert_eq!(out.median.len(), 5);
        assert_eq!(out.deciles.len(), 5);
        println!(
            "Actual TimesFM model {}@{} produced five validated forecast steps",
            out.model, out.revision
        );
    }
    #[tokio::test]
    #[ignore = "requires explicit Yahoo access and the installed TimesFM checkpoint"]
    async fn live_yahoo_timesfm_pipeline_smoke() {
        let request = Request {
            symbol: "AAPL".into(),
            period: "1y".into(),
            interval: "1d".into(),
            horizon: 5,
        };
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(dir.path().into()));
        let accepted = engine.start(request).unwrap();
        assert_eq!(accepted.state, JobState::Running);
        let finished = tokio::time::timeout(StdDuration::from_secs(310), async {
            loop {
                let job = engine.status().job.unwrap();
                if job.state != JobState::Running {
                    break job;
                }
                tokio::time::sleep(StdDuration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(finished.state, JobState::Completed, "{:?}", finished.error);
        let recovered = Engine::new(dir.path().into()).status().job.unwrap();
        assert_eq!(recovered.id, accepted.id);
        assert_eq!(recovered.state, JobState::Completed);
        let view = recovered.result.unwrap();
        assert_eq!(view.forecast.len(), 5);
        assert!(view.history.len() > 32);
        assert!(view.chart_svg.contains("Observed closes"));
        let path = std::env::temp_dir().join("sail-capital-timesfm-live-smoke.json");
        std::fs::write(&path, serde_json::to_vec(&view).unwrap()).unwrap();
        println!(
            "Live Yahoo → TimesFM → Rust result validated: {} bars, {} steps, {}@{}, artifact {}",
            view.context_points,
            view.forecast.len(),
            view.model,
            view.model_revision,
            path.display()
        );
    }
    #[test]
    fn validation_and_calendar_edges() {
        let mut r = Request {
            symbol: " aapl ".into(),
            period: "max".into(),
            interval: "1mo".into(),
            horizon: 1000,
        };
        r.validate().unwrap();
        assert_eq!(r.symbol, "AAPL");
        r.symbol = "../../x".into();
        assert!(r.validate().is_err());
        r.symbol = "AAPL".into();
        r.horizon = 0;
        assert!(r.validate().is_err());
        let end = date("2024-01-31T16:00:00Z");
        assert_eq!(
            calendar_time(end, "1mo", 1).unwrap(),
            date("2024-02-29T16:00:00Z")
        );
        assert_eq!(
            calendar_time(end, "1mo", 2).unwrap(),
            date("2024-03-31T16:00:00Z")
        );
        assert_eq!(
            period_start(date("2024-02-29T16:00:00Z"), "1y").unwrap(),
            date("2023-02-28T16:00:00Z")
        );
        assert_eq!(
            period_start(end, "ytd").unwrap(),
            date("2024-01-01T00:00:00Z")
        );
        assert_eq!(
            calendar_time(end, "90m", 1).unwrap() - end,
            Duration::minutes(90)
        );
    }
    #[test]
    fn worker_results_must_be_complete_and_monotone() {
        let good = serde_json::json!({"model":MODEL,"revision":"a".repeat(40),"median":[5.0],"deciles":[[1.,2.,3.,4.,5.,6.,7.,8.,9.]]});
        assert!(parse_output(&serde_json::to_vec(&good).unwrap(), 1).is_ok());
        assert!(parse_output(&serde_json::to_vec(&good).unwrap(), 2).is_err());
        let mut bad = good.clone();
        bad["deciles"][0][0] = serde_json::json!(10);
        assert!(parse_output(&serde_json::to_vec(&bad).unwrap(), 1).is_err());
        bad = good;
        bad["model"] = serde_json::json!("baseline");
        assert!(parse_output(&serde_json::to_vec(&bad).unwrap(), 1).is_err());
    }
    #[test]
    fn interrupted_jobs_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let e = Engine::new(dir.path().into());
        e.save(Job {
            id: "1".into(),
            request: Request {
                symbol: "AAPL".into(),
                period: "1y".into(),
                interval: "1d".into(),
                horizon: 20,
            },
            state: JobState::Running,
            started_at: Utc::now(),
            finished_at: None,
            error: None,
            result: None,
        })
        .unwrap();
        let restarted = Engine::new(dir.path().into());
        assert_eq!(restarted.status().job.unwrap().state, JobState::Interrupted);
    }
}
