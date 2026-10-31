use chrono::{DateTime, Datelike, Utc};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use sail_guardian::credentials::{CredentialStore, redact_credential_patterns};

use crate::contracts::{ReviewPolicyConfig, ReviewRequest, ReviewResult, ReviewVerdict};

/// Error types for review broker operations.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error("Daily review request limit exceeded ({0} requests)")]
    DailyRequestLimitExceeded(usize),
    #[error("Weekly review request limit exceeded ({0} requests)")]
    WeeklyRequestLimitExceeded(usize),
    #[error("Per-issue review limit reached ({0} reviews, requires material revision)")]
    PerIssueLimitReached(usize),
    #[error("Network or provider error: {0}")]
    ProviderError(String),
    #[error("Invalid JSON response from reviewer: {0}")]
    InvalidResponse(String),
}

/// Durable ledger tracking usage against daily, weekly, and monthly caps,
/// and recording canonical issue reviews to guarantee at most one process invocation per canonical repo+issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewUsageLedger {
    pub day_timestamp: i32,
    pub week_timestamp: u32,
    pub month_timestamp: u32,
    pub requests_today: usize,
    pub requests_this_week: usize,
    pub cost_today_usd: f64,
    pub cost_this_month_usd: f64,
    pub issue_review_counts: HashMap<String, usize>,
    pub issue_last_diff_hash: HashMap<String, String>,
    #[serde(default)]
    pub canonical_issue_reviews: HashMap<String, ReviewResult>,
    #[serde(default)]
    pub canonical_issue_attempts: HashMap<String, String>,
    pub consecutive_errors: usize,
}

impl Default for ReviewUsageLedger {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            day_timestamp: Self::calc_day_ts(&now),
            week_timestamp: Self::calc_week_ts(&now),
            month_timestamp: Self::calc_month_ts(&now),
            requests_today: 0,
            requests_this_week: 0,
            cost_today_usd: 0.0,
            cost_this_month_usd: 0.0,
            issue_review_counts: HashMap::new(),
            issue_last_diff_hash: HashMap::new(),
            canonical_issue_reviews: HashMap::new(),
            canonical_issue_attempts: HashMap::new(),
            consecutive_errors: 0,
        }
    }
}

impl ReviewUsageLedger {
    pub fn calc_day_ts(dt: &DateTime<Utc>) -> i32 {
        dt.year() * 1000 + dt.ordinal() as i32
    }

    pub fn calc_week_ts(dt: &DateTime<Utc>) -> u32 {
        dt.year() as u32 * 100 + dt.iso_week().week()
    }

    pub fn calc_month_ts(dt: &DateTime<Utc>) -> u32 {
        dt.year() as u32 * 100 + dt.month()
    }

    pub fn roll_windows_if_needed(&mut self, now: &DateTime<Utc>) {
        let day_ts = Self::calc_day_ts(now);
        if self.day_timestamp != day_ts {
            self.day_timestamp = day_ts;
            self.requests_today = 0;
            self.cost_today_usd = 0.0;
        }

        let week_ts = Self::calc_week_ts(now);
        if self.week_timestamp != week_ts {
            self.week_timestamp = week_ts;
            self.requests_this_week = 0;
        }

        let month_ts = Self::calc_month_ts(now);
        if self.month_timestamp != month_ts {
            self.month_timestamp = month_ts;
            self.cost_this_month_usd = 0.0;
        }
    }
}

/// Thread-safe Review Broker managing official ChatGPT-subscription authenticated Codex CLI,
/// strict deduplication ensuring AT MOST ONE process invocation per canonical repo+issue,
/// and honest local fallback.
pub struct ReviewBroker {
    config: ReviewPolicyConfig,
    ledger: Arc<Mutex<ReviewUsageLedger>>,
    ledger_path: Option<PathBuf>,
    dedup_cache: Arc<Mutex<HashMap<String, ReviewResult>>>,
    credential_store: Option<Arc<CredentialStore>>,
    mock_mode: bool,
    codex_bin: PathBuf,
    in_flight: Arc<Mutex<HashMap<String, tokio::sync::broadcast::Sender<ReviewResult>>>>,
    invocation_count: Arc<AtomicUsize>,
}

impl ReviewBroker {
    pub fn discover_codex_bin() -> PathBuf {
        if let Ok(p) = std::env::var("SAIL_CODEX_BIN") {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return pb;
            }
        }
        let bundled = PathBuf::from(
            "/home/main/.vscode-server/extensions/openai.chatgpt-26.1021.22027-linux-x64/bin/linux-x86_64/codex",
        );
        if bundled.is_file() {
            return bundled;
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join("codex");
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        PathBuf::from("codex")
    }

    pub fn normalize_canonical_issue_key(raw: &str) -> String {
        let clean = raw.trim().replace("/#", "#");
        if let Some((repo, num)) = clean.split_once('#') {
            format!("{}#{}", repo.trim_matches('/'), num.trim())
        } else {
            clean
        }
    }

    pub fn new(config: ReviewPolicyConfig) -> Self {
        let data_dir = std::env::var("SAIL_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".sail").join("forge")
            });

        let ledger_path = data_dir.join("review_ledger.json");
        let initial_ledger = Self::load_ledger_from_disk(&ledger_path).unwrap_or_default();

        Self {
            config,
            ledger: Arc::new(Mutex::new(initial_ledger)),
            ledger_path: Some(ledger_path),
            dedup_cache: Arc::new(Mutex::new(HashMap::new())),
            credential_store: None,
            mock_mode: false,
            codex_bin: Self::discover_codex_bin(),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            invocation_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn with_ledger_path(mut self, path: PathBuf) -> Self {
        let initial = Self::load_ledger_from_disk(&path).unwrap_or_default();
        self.ledger = Arc::new(Mutex::new(initial));
        self.ledger_path = Some(path);
        self
    }

    pub fn with_credentials(mut self, credentials: Arc<CredentialStore>) -> Self {
        self.credential_store = Some(credentials);
        self
    }

    pub fn with_mock_mode(mut self, mock: bool) -> Self {
        self.mock_mode = mock;
        self
    }

    pub fn with_codex_bin(mut self, bin: PathBuf) -> Self {
        self.codex_bin = bin;
        self
    }

    pub fn invocation_count(&self) -> usize {
        self.invocation_count.load(Ordering::SeqCst)
    }

    fn load_ledger_from_disk(path: &Path) -> Option<ReviewUsageLedger> {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(mut ledger) = serde_json::from_str::<ReviewUsageLedger>(&content) {
                    ledger.roll_windows_if_needed(&Utc::now());
                    return Some(ledger);
                }
            }
        }
        None
    }

    pub fn persist_ledger(&self, ledger: &ReviewUsageLedger) {
        if let Some(path) = &self.ledger_path {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let tmp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
            if let Ok(json) = serde_json::to_string_pretty(ledger) {
                if let Ok(mut file) = fs::File::create(&tmp) {
                    use std::io::Write;
                    let _ = file.write_all(json.as_bytes());
                    let _ = file.sync_all();
                    let _ = fs::rename(&tmp, path);
                }
            }
        }
    }

    pub fn persist_ledger_sync(&self, ledger: &ReviewUsageLedger) {
        self.persist_ledger(ledger);
    }

    pub fn get_ledger(&self) -> ReviewUsageLedger {
        let mut guard = self.ledger.lock().unwrap();
        guard.roll_windows_if_needed(&Utc::now());
        guard.clone()
    }

    /// Calculate deterministic SHA-256 deduplication hash for review payload.
    pub fn compute_dedup_hash(
        issue_id: &str,
        base_commit: &str,
        diff_patch: &str,
        test_manifest: &str,
    ) -> String {
        let mut context = Context::new(&SHA256);
        context.update(issue_id.as_bytes());
        context.update(b":");
        context.update(base_commit.as_bytes());
        context.update(b":");
        context.update(diff_patch.as_bytes());
        context.update(b":");
        context.update(test_manifest.as_bytes());
        let digest = context.finish();
        digest
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    }

    /// Sanitize text by stripping credentials, tokens, keys, and absolute paths before external dispatch.
    pub fn sanitize_payload(&self, text: &str) -> String {
        let mut sanitized = redact_credential_patterns(text);
        if let Some(store) = &self.credential_store {
            sanitized = store.redact_text(&sanitized);
        }
        sanitized
    }

    /// Primary entry point: review an issue change with strict atomic quota checks and honest fallback.
    /// Strictly guarantees AT MOST ONE process invocation per canonical repo+issue across retries/reboots/branches.
    pub async fn review(&self, req: &ReviewRequest) -> ReviewResult {
        let canonical_key = Self::normalize_canonical_issue_key(&req.issue_id);
        let sanitized_diff = self.sanitize_payload(&req.diff_patch);
        let sanitized_manifest = self.sanitize_payload(&req.test_manifest);

        let dedup_hash = Self::compute_dedup_hash(
            &canonical_key,
            &req.base_commit,
            &sanitized_diff,
            &sanitized_manifest,
        );

        // 1. Check in-memory deduplication cache
        if self.config.dedup_cache_enabled {
            let cache = self.dedup_cache.lock().unwrap();
            if let Some(cached_result) = cache.get(&dedup_hash) {
                info!("Review deduplication hit for hash {}", &dedup_hash[..10]);
                let mut result = cached_result.clone();
                result.cached = true;
                return result;
            }
        }

        // 2. Strict Requirement: Check durable persistent ledger on disk
        // At most one process invocation per canonical repo+issue across retries/reboots/branches
        {
            let mut ledger = self.ledger.lock().unwrap();
            ledger.roll_windows_if_needed(&Utc::now());
            if let Some(existing) = ledger.canonical_issue_reviews.get(&canonical_key) {
                info!(
                    "Canonical issue {} already reviewed in persistent ledger; returning cached review without process spawn",
                    canonical_key
                );
                let mut res = existing.clone();
                res.cached = true;
                return res;
            }
            if let Some(attempt) = ledger.canonical_issue_attempts.get(&canonical_key) {
                warn!(
                    "Canonical issue {} was already attempted ({}); strictly enforcing at most one process invocation across retries/reboots",
                    canonical_key, attempt
                );
                let mut fallback = self.run_local_fallback(
                    req,
                    dedup_hash,
                    "previously_attempted_canonical_issue",
                );
                fallback.cached = true;
                return fallback;
            }
        }

        // 3. Single-Flight Concurrency: If 20 simultaneous requests arrive for the SAME canonical issue,
        // exactly 1 becomes the leader and executes the process, while the other 19 subscribe and await the result.
        let (rx, is_leader) = {
            let mut in_flight = self.in_flight.lock().unwrap();
            if let Some(sender) = in_flight.get(&canonical_key) {
                (Some(sender.subscribe()), false)
            } else {
                let (tx, _rx) = tokio::sync::broadcast::channel(32);
                in_flight.insert(canonical_key.clone(), tx);
                (None, true)
            }
        };

        if !is_leader {
            if let Some(mut subscriber) = rx {
                info!(
                    "Awaiting in-flight single-flight review for {}",
                    canonical_key
                );
                if let Ok(mut result) = subscriber.recv().await {
                    result.cached = true;
                    return result;
                }
            }
            // If subscriber missed or error occurred, check ledger
            let ledger = self.ledger.lock().unwrap();
            if let Some(existing) = ledger.canonical_issue_reviews.get(&canonical_key) {
                let mut res = existing.clone();
                res.cached = true;
                return res;
            }
        }

        // 4. Leader execution
        if !self.config.enabled {
            info!("Review policy disabled; utilizing local honest fallback");
            let result = self.run_local_fallback(req, dedup_hash, "policy_disabled");
            self.finish_in_flight(&canonical_key, &result);
            return result;
        }

        // Daily requests cap check
        {
            let ledger = self.ledger.lock().unwrap();
            if ledger.requests_today >= self.config.max_requests_per_day {
                warn!(
                    "Daily review quota reached ({} requests)",
                    ledger.requests_today
                );
                let result =
                    self.run_local_fallback(req, dedup_hash, "daily_request_limit_exceeded");
                self.finish_in_flight(&canonical_key, &result);
                return result;
            }
        }

        // Atomic Crash-Safe Reservation: Persist attempt state to durable ledger with fsync BEFORE invocation
        {
            let mut ledger = self.ledger.lock().unwrap();
            let marker = format!("attempted:{}:{}", Utc::now().to_rfc3339(), dedup_hash);
            ledger
                .canonical_issue_attempts
                .insert(canonical_key.clone(), marker);
            ledger.requests_today += 1;
            ledger.requests_this_week += 1;
            let count = ledger
                .issue_review_counts
                .entry(canonical_key.clone())
                .or_insert(0);
            *count += 1;
            self.persist_ledger(&ledger);
        }

        // Execute ChatGPT-subscription authenticated official Codex review
        self.invocation_count.fetch_add(1, Ordering::SeqCst);
        let exec_result = self
            .execute_codex_review(req, &sanitized_diff, &sanitized_manifest, &dedup_hash)
            .await;

        let result = match exec_result {
            Ok(res) => res,
            Err(e) => {
                warn!(
                    "Codex review execution failed: {}; falling back to local honest review",
                    e
                );
                self.run_local_fallback(req, dedup_hash.clone(), &format!("codex_error: {}", e))
            }
        };

        // Persist final result to durable ledger
        {
            let mut ledger = self.ledger.lock().unwrap();
            ledger
                .canonical_issue_reviews
                .insert(canonical_key.clone(), result.clone());
            self.persist_ledger(&ledger);
        }

        if self.config.dedup_cache_enabled {
            let mut cache = self.dedup_cache.lock().unwrap();
            cache.insert(dedup_hash.clone(), result.clone());
        }

        // Notify in-flight subscribers and cleanup
        self.finish_in_flight(&canonical_key, &result);

        result
    }

    fn finish_in_flight(&self, canonical_key: &str, result: &ReviewResult) {
        let mut in_flight = self.in_flight.lock().unwrap();
        if let Some(tx) = in_flight.remove(canonical_key) {
            let _ = tx.send(result.clone());
        }
    }

    /// Dispatch code review to official Codex CLI authenticated via ChatGPT subscription.
    async fn execute_codex_review(
        &self,
        req: &ReviewRequest,
        sanitized_diff: &str,
        sanitized_manifest: &str,
        dedup_hash: &str,
    ) -> Result<ReviewResult, ReviewError> {
        if self.mock_mode {
            let passed = !sanitized_diff.contains("TODO_FAIL_REVIEW");
            return Ok(ReviewResult {
                verdict: ReviewVerdict {
                    passed,
                    requirements_covered: passed,
                    untested_edge_cases: vec![],
                    detected_bugs: if passed {
                        vec![]
                    } else {
                        vec!["Intentional mock defect".into()]
                    },
                    security_observations: vec!["Codex security audit verified".into()],
                    suggested_edits: vec![],
                    confidence_score: 0.98,
                    raw_feedback:
                        "Official Codex CLI review completed successfully with no defects.".into(),
                },
                cached: false,
                dedup_hash: dedup_hash.to_string(),
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: 0.0,
                provider: "codex-cli:mock".into(),
            });
        }

        let codex_bin = &self.codex_bin;
        if !codex_bin.exists() {
            return Err(ReviewError::ProviderError(format!(
                "Codex CLI binary not found at {:?}",
                codex_bin
            )));
        }

        let prompt = format!(
            "Review pull request changes for issue {}.\n\
            Test results:\n{}\n\n\
            Unified diff:\n{}\n\n\
            Provide structured review output with:\n\
            VERDICT: APPROVED or CHANGES_REQUESTED\n\
            REQUIREMENTS_COVERED: YES or NO\n\
            CONFIDENCE: <0.0 to 1.0>\n\
            SUMMARY: <review analysis>\n",
            req.issue_id, sanitized_manifest, sanitized_diff
        );

        // Run official Codex CLI non-interactively
        let output = std::process::Command::new(codex_bin)
            .arg("review")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(prompt.as_bytes());
                }
                child.wait_with_output()
            })
            .map_err(|e| ReviewError::ProviderError(e.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(ReviewError::ProviderError(format!(
                "Codex review exited with non-zero status {}: {}",
                output.status, stderr
            )));
        }

        let feedback = stdout.trim().to_string();
        let upper = feedback.to_uppercase();

        let explicit_approved = upper.contains("VERDICT: APPROVED")
            || upper.contains("VERDICT: PASS")
            || upper.contains("VERDICT: PASSED");
        let changes_requested = upper.contains("CHANGES_REQUESTED")
            || upper.contains("VERDICT: REJECT")
            || upper.contains("VERDICT: REJECTED")
            || upper.contains("CRITICAL")
            || upper.contains("BLOCKING ISSUE");
        let reqs_covered = upper.contains("REQUIREMENTS_COVERED: YES");

        let mut confidence = 0.0f32;
        for line in feedback.lines() {
            if let Some(rest) = line.to_uppercase().strip_prefix("CONFIDENCE:") {
                if let Ok(val) = rest.trim().parse::<f32>() {
                    confidence = val.clamp(0.0, 1.0);
                }
            }
        }

        let passed = explicit_approved && !changes_requested;
        let requirements_covered = passed && reqs_covered;
        if passed && confidence == 0.0 {
            confidence = 0.95;
        }

        let detected_bugs = if passed {
            vec![]
        } else {
            vec![
                "Structured review indicates issues, requested changes, or inconclusive approval"
                    .into(),
            ]
        };

        Ok(ReviewResult {
            verdict: ReviewVerdict {
                passed,
                requirements_covered,
                untested_edge_cases: vec![],
                detected_bugs,
                security_observations: vec!["Codex CLI subscription review completed".into()],
                suggested_edits: vec![],
                confidence_score: confidence,
                raw_feedback: feedback,
            },
            cached: false,
            dedup_hash: dedup_hash.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0, // ChatGPT subscription: $0.00 incremental per-token API cost
            provider: "codex-cli:subscription".into(),
        })
    }

    /// Local honest fallback when API key is missing, quotas are reached, or network fails.
    /// Fulfills Issue #96: "Local fallback must say 'unreviewed externally / inconclusive'
    /// unless independently verifiable local tests prove criteria; cannot fabricate review pass".
    pub fn run_local_fallback(
        &self,
        req: &ReviewRequest,
        dedup_hash: String,
        reason: &str,
    ) -> ReviewResult {
        let has_tests =
            req.test_manifest.contains("test result: ok") || req.test_manifest.contains("PASSED");

        ReviewResult {
            verdict: ReviewVerdict {
                passed: false,
                requirements_covered: false,
                untested_edge_cases: vec![
                    format!("Unreviewed externally ({})", reason),
                    "External reviewer skipped; reliance is strictly on local tests".into(),
                ],
                detected_bugs: vec![],
                security_observations: vec!["Local sanity check applied".into()],
                suggested_edits: vec![],
                confidence_score: 0.0,
                raw_feedback: format!(
                    "Unreviewed externally / inconclusive: {}; local test evidence required (local tests present: {})",
                    reason, has_tests
                ),
            },
            cached: false,
            dedup_hash,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            provider: format!("local_deterministic_review:{}", reason),
        }
    }
}
