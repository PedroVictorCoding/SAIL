//! Model signals (round 12): views from machine-learning models, run as Python helpers,
//! fed to `signals` as [`crate::signals::ExternalScores`]. They get exactly the same
//! out-of-sample IC check as the price signals before they get any weight.
//!
//! Each model is opt-in through `SAIL_MODEL_SIGNALS` (a comma list, e.g.
//! `chronos,yolo`). Its helper runs with `SAIL_SIGNALS_PYTHON` (default
//! [`DEFAULT_PYTHON`], made by `scripts/signals/setup.sh`) from `SAIL_SIGNALS_SCRIPTS`
//! (default [`DEFAULT_SCRIPTS`]). Helpers read prices on stdin and write values on stdout.
//! They download their model weights once from Hugging Face. No holdings, amounts or
//! symbols leave the host: the helper gets instrument keys and prices only.
//!
//! **Look-ahead.** A pretrained model may have seen the very prices it is scored on.
//! Values dated before a model's `evaluate_from` are never scored. The latest value
//! (used for today's view) is always after it.
//!
//! **Protocol.** stdin:
//! `{"horizon": h, "items": [{"key", "origins": [date], "bars": [[date, open|null,
//! high|null, low|null, close]]}]}`, with bars oldest first. A helper must use only bars
//! up to each origin.
//! stdout: `{"model_version": str, "values": [[key, date, value]], "skipped": n,
//! "notes": [str]}`, or `{"error": str}`.
use crate::signals::SignalId;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Default interpreter, inside the environment `scripts/signals/setup.sh` makes.
pub const DEFAULT_PYTHON: &str = "/srv/sail-data/venvs/signals/bin/python";

/// Default helper directory, relative to SAIL's working directory.
pub const DEFAULT_SCRIPTS: &str = "scripts/signals";

/// Default cache directory for model weights.
pub const DEFAULT_CACHE: &str = "/srv/sail-data/models/signals";

/// Largest helper output read.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Longest one helper run may take.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(3 * 60 * 60);

/// Sessions between scored origins (about weekly).
pub const STEP_SESSIONS: usize = 5;

/// Fewest closes before the first origin.
pub const MIN_HISTORY: usize = 252;

/// One model signal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Short name used in `SAIL_MODEL_SIGNALS`.
    pub name: String,
    /// The signal it feeds.
    pub signal: SignalId,
    /// Helper script file name.
    pub script: String,
    /// What it is, in one line.
    pub summary: String,
    /// The model it runs, as published.
    pub model: String,
    /// Licence of the weights, as published ("not stated" when the card has none).
    pub license: String,
    /// Values before this date are never scored (the model may have seen those prices).
    pub evaluate_from: NaiveDate,
    /// Horizon in sessions the helper is asked about.
    pub horizon: u32,
    /// Whether it needs open, high and low (instruments without them are skipped).
    pub needs_ohl: bool,
}

/// The known model signals.
pub fn specs() -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            name: "chronos".into(),
            signal: SignalId::FoundationForecast,
            script: "chronos_signal.py".into(),
            summary: "Median 60-session return forecast by a pretrained time-series model".into(),
            model: "amazon/chronos-bolt-small".into(),
            license: "Apache-2.0".into(),
            // Chronos-Bolt was published in late November 2024.
            evaluate_from: NaiveDate::from_ymd_opt(2024, 12, 1).expect("date"),
            horizon: 60,
            needs_ohl: false,
        },
        ModelSpec {
            name: "yolo".into(),
            signal: SignalId::ChartPattern,
            script: "yolo_signal.py".into(),
            summary: "Chart patterns found by YOLOv8 on a rendered 120-session candlestick chart"
                .into(),
            model: "foduucom/stockmarket-pattern-detection-yolov8".into(),
            license: "not stated (Ultralytics runtime is AGPL-3.0); personal use only".into(),
            // Its training charts are undated; score only after its weights last changed
            // (2025-04-02 on Hugging Face).
            evaluate_from: NaiveDate::from_ymd_opt(2025, 4, 15).expect("date"),
            horizon: 60,
            needs_ohl: true,
        },
        ModelSpec {
            name: "yolo_trend".into(),
            signal: SignalId::ChartTrend,
            script: "yolo_trend_signal.py".into(),
            summary: "Up/Down trend found by YOLOv8 on a rendered 120-session candlestick chart"
                .into(),
            model: "foduucom/stockmarket-future-prediction".into(),
            license: "not stated (Ultralytics runtime is AGPL-3.0); personal use only".into(),
            // Its weights last changed on 2023-10-07.
            evaluate_from: NaiveDate::from_ymd_opt(2023, 11, 1).expect("date"),
            horizon: 60,
            needs_ohl: true,
        },
    ]
}

/// A model that gives an outlook component (a quantile path) rather than a signal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlookSpec {
    /// Short name used in `SAIL_MODEL_SIGNALS` (shared with signals: `chronos` turns on
    /// both).
    pub name: String,
    /// The component it feeds.
    pub component: crate::outlook::ComponentId,
    /// Helper script file name.
    pub script: String,
    /// What it is, in one line.
    pub summary: String,
    /// The model it runs, as published.
    pub model: String,
    /// Licence of the weights or library.
    pub license: String,
    /// Paths before this origin are never used (look-ahead from pretraining).
    pub evaluate_from: NaiveDate,
    /// Horizons it answers.
    pub horizons: Vec<u32>,
}

/// How far back outlook helpers are asked about: enough for 156 weekly origins of
/// weights before each of three years of monthly cutoffs.
pub const OUTLOOK_HISTORY_DAYS: i64 = 6 * 366;

/// The known outlook models.
pub fn outlook_specs() -> Vec<OutlookSpec> {
    vec![
        OutlookSpec {
            name: "chronos".into(),
            component: crate::outlook::ComponentId::Chronos,
            script: "chronos_outlook.py".into(),
            summary: "Quantile path of a pretrained time-series model (up to 64 sessions)".into(),
            model: "amazon/chronos-bolt-small".into(),
            license: "Apache-2.0".into(),
            evaluate_from: NaiveDate::from_ymd_opt(2024, 12, 1).expect("date"),
            horizons: vec![21, 63],
        },
        OutlookSpec {
            name: "gbdt".into(),
            component: crate::outlook::ComponentId::Gbdt,
            script: "gbdt_outlook.py".into(),
            summary: "Gradient-boosted quantile regression on price features, refit monthly on past data only"
                .into(),
            model: "scikit-learn HistGradientBoostingRegressor (quantile loss)".into(),
            license: "BSD-3-Clause".into(),
            // Trained walk-forward by SAIL itself, so no pretraining look-ahead.
            evaluate_from: NaiveDate::MIN,
            horizons: crate::outlook::HORIZONS.to_vec(),
        },
    ]
}

/// The outlook models the owner turned on with `SAIL_MODEL_SIGNALS`.
pub fn outlook_enabled_from_env() -> Vec<OutlookSpec> {
    let wanted = wanted_names();
    outlook_specs()
        .into_iter()
        .filter(|s| wanted.contains(&s.name))
        .collect()
}

fn wanted_names() -> Vec<String> {
    std::env::var("SAIL_MODEL_SIGNALS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Weekly origins for an outlook helper: every [`STEP_SESSIONS`]-th session counted back
/// from the latest, from index [`MIN_HISTORY`], within [`OUTLOOK_HISTORY_DAYS`] of the
/// latest session and on or after `from`.
pub fn outlook_origins(sessions: &[NaiveDate], from: NaiveDate) -> Vec<NaiveDate> {
    let Some(latest) = sessions.last() else {
        return Vec::new();
    };
    let floor = (*latest - chrono::Duration::days(OUTLOOK_HISTORY_DAYS)).max(from);
    origins(sessions, floor)
}

/// The specs the owner turned on with `SAIL_MODEL_SIGNALS`.
pub fn enabled_from_env() -> Vec<ModelSpec> {
    let wanted = wanted_names();
    specs()
        .into_iter()
        .filter(|s| wanted.contains(&s.name))
        .collect()
}

/// `[date, open, high, low, close]`.
pub type HelperBar = (NaiveDate, Option<f64>, Option<f64>, Option<f64>, f64);

/// One instrument's input to a helper.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HelperItem {
    /// Ledger instrument key.
    pub key: String,
    /// Sessions to produce a value at.
    pub origins: Vec<NaiveDate>,
    /// `[date, open, high, low, close]`, oldest first. Prices as `f64`.
    pub bars: Vec<HelperBar>,
}

#[derive(Serialize)]
struct HelperInput<'a> {
    horizon: u32,
    items: &'a [HelperItem],
}

/// What a helper returned.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct HelperOutput {
    /// The exact model and version, e.g. `amazon/chronos-bolt-small@<revision>`.
    pub model_version: String,
    /// `(instrument key, session, raw value)`.
    pub values: Vec<(String, NaiveDate, f64)>,
    /// Origins it couldn't score.
    #[serde(default)]
    pub skipped: u32,
    /// Plain notes.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Why a helper run failed. Messages are safe to show.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// The interpreter or script couldn't start.
    #[error("the {0} helper could not start; run scripts/signals/setup.sh")]
    NotInstalled(String),
    /// It reported an error.
    #[error("the {0} helper failed: {1}")]
    Failed(String, String),
    /// Output over the cap, unreadable, or a timeout.
    #[error("the {0} helper's output could not be used: {1}")]
    BadOutput(String, String),
}

/// The origins to compute for one instrument with `n` closes (sessions given): every
/// [`STEP_SESSIONS`]-th session counted back from the latest, from index
/// [`MIN_HISTORY`] on, on or after `from`. The latest session is always included when it
/// qualifies.
pub fn origins(sessions: &[NaiveDate], from: NaiveDate) -> Vec<NaiveDate> {
    let n = sessions.len();
    if n <= MIN_HISTORY {
        return Vec::new();
    }
    let mut out: Vec<NaiveDate> = (MIN_HISTORY..n)
        .filter(|i| (n - 1 - i).is_multiple_of(STEP_SESSIONS))
        .map(|i| sessions[i])
        .filter(|d| *d >= from)
        .collect();
    out.sort();
    out
}

/// Runs a signal helper. `python`, `scripts` and `cache` come from the environment in
/// the service and are explicit here for tests.
pub async fn run_helper(
    spec: &ModelSpec,
    python: &str,
    scripts: &str,
    cache: Option<&str>,
    items: &[HelperItem],
) -> Result<HelperOutput, ModelError> {
    let input = serde_json::to_vec(&HelperInput {
        horizon: spec.horizon,
        items,
    })
    .map_err(|e| ModelError::BadOutput(spec.name.clone(), e.to_string()))?;
    let body = run_json(&spec.name, &spec.script, python, scripts, cache, input).await?;
    parse_output(&spec.name, &body)
}

/// What an outlook helper returned.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PathOutput {
    /// The exact model and version.
    pub model_version: String,
    /// `(instrument key, origin, horizon, step, [q05, q16, q50, q84, q95])`.
    pub paths: Vec<(String, NaiveDate, u32, u32, [f64; 5])>,
    /// Origins it couldn't answer.
    #[serde(default)]
    pub skipped: u32,
    /// Plain notes.
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Serialize)]
struct PathInput<'a> {
    horizons: &'a [u32],
    steps: std::collections::BTreeMap<String, Vec<u32>>,
    items: &'a [HelperItem],
}

/// Runs an outlook helper and groups its rows into paths.
pub async fn run_outlook_helper(
    spec: &OutlookSpec,
    python: &str,
    scripts: &str,
    cache: Option<&str>,
    items: &[HelperItem],
) -> Result<(String, Vec<crate::outlook::ExternalPath>, u32, Vec<String>), ModelError> {
    let steps = spec
        .horizons
        .iter()
        .map(|h| (h.to_string(), crate::outlook::steps(*h)))
        .collect();
    let input = serde_json::to_vec(&PathInput {
        horizons: &spec.horizons,
        steps,
        items,
    })
    .map_err(|e| ModelError::BadOutput(spec.name.clone(), e.to_string()))?;
    let body = run_json(&spec.name, &spec.script, python, scripts, cache, input).await?;
    let out = parse_path_output(&spec.name, &body)?;
    Ok((
        out.model_version.clone(),
        group_paths(spec.component, &out),
        out.skipped,
        out.notes,
    ))
}

/// Reads an outlook helper's stdout.
pub fn parse_path_output(name: &str, body: &[u8]) -> Result<PathOutput, ModelError> {
    let value = parse_value(name, body)?;
    let mut out: PathOutput = serde_json::from_value(value)
        .map_err(|e| ModelError::BadOutput(name.into(), e.to_string()))?;
    out.paths
        .retain(|(_, _, h, t, q)| *t >= 1 && t <= h && q.iter().all(|v| v.is_finite()));
    Ok(out)
}

/// Rows into one `ExternalPath` per (instrument, origin, horizon), steps increasing,
/// each row's quantiles sorted so they never cross.
pub fn group_paths(
    component: crate::outlook::ComponentId,
    out: &PathOutput,
) -> Vec<crate::outlook::ExternalPath> {
    type Rows = Vec<(u32, [f64; 5])>;
    let mut grouped: std::collections::BTreeMap<(String, NaiveDate, u32), Rows> =
        Default::default();
    for (key, origin, horizon, step, q) in &out.paths {
        let mut q = *q;
        q.sort_by(f64::total_cmp);
        grouped
            .entry((key.clone(), *origin, *horizon))
            .or_default()
            .push((*step, q));
    }
    grouped
        .into_iter()
        .map(|((key, origin, horizon), mut rows)| {
            rows.sort_by_key(|(t, _)| *t);
            rows.dedup_by_key(|(t, _)| *t);
            crate::outlook::ExternalPath {
                component,
                instrument_key: key,
                origin,
                horizon,
                path: crate::outlook::PathQuantiles {
                    steps: rows.iter().map(|(t, _)| *t).collect(),
                    values: rows.into_iter().map(|(_, q)| q).collect(),
                },
            }
        })
        .collect()
}

async fn run_json(
    name: &str,
    script: &str,
    python: &str,
    scripts: &str,
    cache: Option<&str>,
    input: Vec<u8>,
) -> Result<Vec<u8>, ModelError> {
    let name = name.to_string();
    let script = std::path::Path::new(scripts).join(script);
    let mut command = tokio::process::Command::new(python);
    command
        .arg("-I")
        .arg(&script)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for var in [
        "PATH",
        "HOME",
        "LANG",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTPS_PROXY",
        "https_proxy",
        "NO_PROXY",
    ] {
        if let Ok(value) = std::env::var(var) {
            command.env(var, value);
        }
    }
    if let Some(cache) = cache
        && std::fs::create_dir_all(cache).is_ok()
    {
        command.env("HF_HOME", cache);
        command.env("XDG_CACHE_HOME", cache);
        command.env("YOLO_CONFIG_DIR", cache);
        command.env("MPLCONFIGDIR", cache);
    }
    // No usage analytics from the libraries.
    for (var, value) in [
        ("HF_HUB_DISABLE_TELEMETRY", "1"),
        ("DO_NOT_TRACK", "1"),
        ("YOLO_OFFLINE", "1"),
    ] {
        command.env(var, value);
    }
    let mut child = command
        .spawn()
        .map_err(|_| ModelError::NotInstalled(name.clone()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ModelError::NotInstalled(name.clone()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| ModelError::NotInstalled(name.clone()))?;
    let work = async {
        let writer = async {
            let _ = stdin.write_all(&input).await;
            drop(stdin);
        };
        let reader = async {
            let mut body = Vec::new();
            (&mut stdout)
                .take(MAX_OUTPUT_BYTES as u64 + 1)
                .read_to_end(&mut body)
                .await
                .map(|_| body)
        };
        let ((), body) = tokio::join!(writer, reader);
        let _ = child.wait().await;
        body
    };
    match tokio::time::timeout(RUN_TIMEOUT, work).await {
        Err(_) => Err(ModelError::BadOutput(name, "it timed out".into())),
        Ok(Err(e)) => Err(ModelError::BadOutput(name, e.to_string())),
        Ok(Ok(body)) => Ok(body),
    }
}

fn parse_value(name: &str, body: &[u8]) -> Result<serde_json::Value, ModelError> {
    if body.len() > MAX_OUTPUT_BYTES {
        return Err(ModelError::BadOutput(name.into(), "too large".into()));
    }
    if body.is_empty() {
        return Err(ModelError::NotInstalled(name.into()));
    }
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| ModelError::BadOutput(name.into(), "not JSON".into()))?;
    if let Some(error) = value.get("error").and_then(|e| e.as_str()) {
        return Err(ModelError::Failed(name.into(), error.to_string()));
    }
    Ok(value)
}

/// Reads a helper's stdout.
pub fn parse_output(name: &str, body: &[u8]) -> Result<HelperOutput, ModelError> {
    let value = parse_value(name, body)?;
    let mut out: HelperOutput = serde_json::from_value(value)
        .map_err(|e| ModelError::BadOutput(name.into(), e.to_string()))?;
    out.values.retain(|(_, _, v)| v.is_finite());
    Ok(out)
}
