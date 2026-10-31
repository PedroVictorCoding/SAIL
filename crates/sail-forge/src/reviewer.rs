use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use chrono::{DateTime, Datelike, Utc};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use sail_guardian::credentials::{redact_credential_patterns, CredentialStore, SecretString};
use sail_common::types::SubsystemId;

use crate::contracts::{ReviewPolicyConfig, ReviewRequest, ReviewResult, ReviewVerdict};

/// Error types for review broker operations.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error("Daily review request limit exceeded ({0} requests)")]
    DailyRequestLimitExceeded(usize),
    #[error("Weekly review request limit exceeded ({0} requests)")]
    WeeklyRequestLimitExceeded(usize),
    #[error("Daily cost budget exceeded (${current:.2} + ${estimated:.2} > ${limit:.2})")]
    DailyBudgetExceeded {
        current: f64,
        estimated: f64,
        limit: f64,
    },
    #[error("Monthly cost budget exceeded (${current:.2} + ${estimated:.2} > ${limit:.2})")]
    MonthlyBudgetExceeded {
        current: f64,
        estimated: f64,
        limit: f64,
    },
    #[error("Per-issue review limit reached ({0} reviews, requires material revision)")]
    PerIssueLimitReached(usize),
    #[error("Circuit breaker is tripped due to consecutive provider errors ({0})")]
    CircuitBreakerTripped(usize),
    #[error("HTTP 429 rate limit received from provider; cooling down")]
    RateLimited429,
    #[error("Network or provider error: {0}")]
    ProviderError(String),
    #[error("Invalid JSON response from reviewer: {0}")]
    InvalidResponse(String),
}

/// Durable ledger tracking usage against daily, weekly, and monthly caps.
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

/// Thread-safe Review Broker managing quotas, deduplication, secret redaction,
/// atomic pre-flight reservations, and honest local fallback.
pub struct ReviewBroker {
    config: ReviewPolicyConfig,
    ledger: Arc<Mutex<ReviewUsageLedger>>,
    ledger_path: Option<PathBuf>,
    dedup_cache: Arc<Mutex<HashMap<String, ReviewResult>>>,
    credential_store: Option<Arc<CredentialStore>>,
    mock_mode: bool,
}

impl ReviewBroker {
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

    fn persist_ledger(&self, ledger: &ReviewUsageLedger) {
        if let Some(path) = &self.ledger_path {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let tmp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
            if let Ok(json) = serde_json::to_string_pretty(ledger) {
                if fs::write(&tmp, json).is_ok() {
                    let _ = fs::rename(&tmp, path);
                }
            }
        }
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

    /// Estimate cost in USD across the entire sanitized payload:
    /// prompt framing + issue text + test manifest + diff patch + output token reservation.
    pub fn estimate_total_payload_cost(
        issue_id: &str,
        test_manifest: &str,
        diff_patch: &str,
        max_output_tokens: u32,
    ) -> (u32, f64) {
        // Conservative character-to-token ratio (3 characters per token)
        let total_chars = issue_id.len() + test_manifest.len() + diff_patch.len() + 1024;
        let in_tokens = (total_chars as f64 / 3.0).ceil() as u32;

        let input_cost = (in_tokens as f64 / 1_000_000.0) * 0.15;
        let output_cost = (max_output_tokens as f64 / 1_000_000.0) * 0.60;
        (in_tokens, input_cost + output_cost)
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
    pub async fn review(&self, req: &ReviewRequest) -> ReviewResult {
        let sanitized_diff = self.sanitize_payload(&req.diff_patch);
        let sanitized_manifest = self.sanitize_payload(&req.test_manifest);

        let dedup_hash = Self::compute_dedup_hash(
            &req.issue_id,
            &req.base_commit,
            &sanitized_diff,
            &sanitized_manifest,
        );

        // 1. Check deduplication cache
        if self.config.dedup_cache_enabled {
            let cache = self.dedup_cache.lock().unwrap();
            if let Some(cached_result) = cache.get(&dedup_hash) {
                info!("Review deduplication hit for hash {}", &dedup_hash[..10]);
                let mut result = cached_result.clone();
                result.cached = true;
                return result;
            }
        }

        // 2. Check if external review is enabled
        if !self.config.enabled {
            info!("Review policy disabled; utilizing local honest fallback");
            return self.run_local_fallback(req, dedup_hash, "policy_disabled");
        }

        // 3. Resolve API key securely from CredentialStore or environment
        let api_key = self.resolve_api_key();
        if api_key.is_none() && !self.mock_mode {
            info!("OpenAI API key unavailable; gracefully falling back to local review");
            return self.run_local_fallback(req, dedup_hash, "missing_api_key");
        }

        // 4. Atomic Pre-Flight Reservation (decrements quota BEFORE making external call)
        let (in_tokens, est_cost) = Self::estimate_total_payload_cost(
            &req.issue_id,
            &sanitized_manifest,
            &sanitized_diff,
            self.config.max_output_tokens_per_review,
        );

        if in_tokens > self.config.max_input_tokens_per_review {
            warn!("Payload tokens ({}) exceeds configured maximum ({})", in_tokens, self.config.max_input_tokens_per_review);
            return self.run_local_fallback(req, dedup_hash, "input_token_limit_exceeded");
        }

        let reservation = self.atomic_reserve_quota(&req.issue_id, &dedup_hash, est_cost);
        match reservation {
            Ok(()) => {
                // Execute review against API (or mock)
                let exec_result = self.execute_api_review(req, &sanitized_diff, &sanitized_manifest, &dedup_hash, api_key).await;
                match exec_result {
                    Ok(result) => {
                        // Reconcile actual usage against reserved cost
                        self.reconcile_usage(
                            &req.issue_id,
                            &dedup_hash,
                            est_cost,
                            result.cost_usd,
                            true,
                        );

                        if self.config.dedup_cache_enabled {
                            let mut cache = self.dedup_cache.lock().unwrap();
                            cache.insert(dedup_hash.clone(), result.clone());
                        }

                        result
                    }
                    Err(e) => {
                        warn!("API review encountered error: {}; falling back to local review", e);
                        let is_429 = matches!(e, ReviewError::RateLimited429);
                        self.reconcile_usage(
                            &req.issue_id,
                            &dedup_hash,
                            est_cost,
                            0.0,
                            false,
                        );
                        if is_429 {
                            let mut ledger = self.ledger.lock().unwrap();
                            ledger.consecutive_errors = 3; // Trip circuit breaker
                            self.persist_ledger(&ledger);
                        }
                        self.run_local_fallback(req, dedup_hash, &format!("error_fallback: {}", e))
                    }
                }
            }
            Err(e) => {
                warn!("Review quota check prevented external call: {}; falling back to local review", e);
                self.run_local_fallback(req, dedup_hash, &format!("quota_capped: {}", e))
            }
        }
    }

    /// Atomically check quotas and immediately reserve the request slot and estimated cost in durable storage.
    fn atomic_reserve_quota(
        &self,
        issue_id: &str,
        current_diff_hash: &str,
        estimated_cost: f64,
    ) -> Result<(), ReviewError> {
        let mut ledger = self.ledger.lock().unwrap();
        ledger.roll_windows_if_needed(&Utc::now());

        // Circuit breaker: max 3 consecutive errors
        if ledger.consecutive_errors >= 3 {
            return Err(ReviewError::CircuitBreakerTripped(ledger.consecutive_errors));
        }

        // Daily requests cap
        if ledger.requests_today >= self.config.max_requests_per_day {
            return Err(ReviewError::DailyRequestLimitExceeded(ledger.requests_today));
        }

        // Weekly requests cap
        if ledger.requests_this_week >= self.config.max_requests_per_week {
            return Err(ReviewError::WeeklyRequestLimitExceeded(ledger.requests_this_week));
        }

        // Per-issue review count
        let count = ledger.issue_review_counts.get(issue_id).copied().unwrap_or(0);
        if count >= self.config.max_reviews_per_issue {
            if let Some(last_hash) = ledger.issue_last_diff_hash.get(issue_id) {
                if last_hash == current_diff_hash || count >= 2 {
                    return Err(ReviewError::PerIssueLimitReached(count));
                }
            } else if count >= 2 {
                return Err(ReviewError::PerIssueLimitReached(count));
            }
        }

        // Daily budget cap ($2.00 default)
        if ledger.cost_today_usd + estimated_cost > self.config.max_cost_per_day_usd {
            return Err(ReviewError::DailyBudgetExceeded {
                current: ledger.cost_today_usd,
                estimated: estimated_cost,
                limit: self.config.max_cost_per_day_usd,
            });
        }

        // Monthly budget cap ($10.00 default - supersedes daily)
        if ledger.cost_this_month_usd + estimated_cost > self.config.max_cost_per_month_usd {
            return Err(ReviewError::MonthlyBudgetExceeded {
                current: ledger.cost_this_month_usd,
                estimated: estimated_cost,
                limit: self.config.max_cost_per_month_usd,
            });
        }

        // ATOMIC PRE-FLIGHT RESERVATION: deduct funds immediately
        ledger.requests_today += 1;
        ledger.requests_this_week += 1;
        ledger.cost_today_usd += estimated_cost;
        ledger.cost_this_month_usd += estimated_cost;
        *ledger.issue_review_counts.entry(issue_id.to_string()).or_insert(0) += 1;
        ledger.issue_last_diff_hash.insert(issue_id.to_string(), current_diff_hash.to_string());

        // Immediately persist updated reservation to disk
        self.persist_ledger(&ledger);
        Ok(())
    }

    /// Reconcile actual token cost against initial reservation.
    fn reconcile_usage(
        &self,
        _issue_id: &str,
        _diff_hash: &str,
        reserved_cost: f64,
        actual_cost: f64,
        success: bool,
    ) {
        let mut ledger = self.ledger.lock().unwrap();
        ledger.roll_windows_if_needed(&Utc::now());

        let delta = actual_cost - reserved_cost;
        ledger.cost_today_usd = (ledger.cost_today_usd + delta).max(0.0);
        ledger.cost_this_month_usd = (ledger.cost_this_month_usd + delta).max(0.0);

        if success {
            ledger.consecutive_errors = 0;
        } else {
            ledger.consecutive_errors += 1;
        }

        self.persist_ledger(&ledger);
    }

    fn resolve_api_key(&self) -> Option<SecretString> {
        if let Some(store) = &self.credential_store {
            if let Some(val) = store.get_secret(SubsystemId::Forge, "openai_api_key") {
                return Some(SecretString::new(val));
            }
        }
        if let Ok(key) = std::env::var("SAIL_OPENAI_API_KEY") {
            if !key.trim().is_empty() {
                return Some(SecretString::new(key));
            }
        }
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            if !key.trim().is_empty() {
                return Some(SecretString::new(key));
            }
        }
        None
    }

    /// Dispatch sanitized request to official OpenAI API.
    async fn execute_api_review(
        &self,
        req: &ReviewRequest,
        sanitized_diff: &str,
        sanitized_manifest: &str,
        dedup_hash: &str,
        api_key: Option<SecretString>,
    ) -> Result<ReviewResult, ReviewError> {
        if self.mock_mode {
            let passed = !sanitized_diff.contains("TODO_FAIL_REVIEW");
            return Ok(ReviewResult {
                verdict: ReviewVerdict {
                    passed,
                    requirements_covered: true,
                    untested_edge_cases: vec![],
                    detected_bugs: if passed { vec![] } else { vec!["Intentional mock defect".into()] },
                    security_observations: vec!["No credential leaks observed in sanitized diff".into()],
                    suggested_edits: vec![],
                    confidence_score: 0.95,
                    raw_feedback: "Mock automated review completed successfully".into(),
                },
                cached: false,
                dedup_hash: dedup_hash.to_string(),
                input_tokens: 1200,
                output_tokens: 350,
                cost_usd: 0.00039,
                provider: "openai_mock".into(),
            });
        }

        let key = api_key.ok_or_else(|| ReviewError::ProviderError("No API key available".into()))?;

        let prompt = format!(
            "You are a strict code review assistant for the SAIL system. Review this pull request diff for issue {}.\n\
            Analyze: requirement coverage, untested edge cases, bugs, security observations, and suggested edits.\n\
            Return strictly valid JSON adhering to this schema:\n\
            {{\n\
              \"passed\": boolean,\n\
              \"requirements_covered\": boolean,\n\
              \"untested_edge_cases\": [string],\n\
              \"detected_bugs\": [string],\n\
              \"security_observations\": [string],\n\
              \"suggested_edits\": [string],\n\
              \"confidence_score\": float (0.0 to 1.0),\n\
              \"raw_feedback\": string\n\
            }}\n\n\
            Test Manifest:\n{}\n\n\
            Unified Diff:\n{}",
            req.issue_id, sanitized_manifest, sanitized_diff
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| ReviewError::ProviderError(e.to_string()))?;

        let payload = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": "You are a code review assistant. Output JSON only."},
                {"role": "user", "content": prompt}
            ],
            "response_format": {"type": "json_object"},
            "max_tokens": self.config.max_output_tokens_per_review,
            "temperature": 0.2
        });

        let resp = client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", key.expose_secret()))
            .json(&payload)
            .send()
            .await
            .map_err(|e| ReviewError::ProviderError(e.to_string()))?;

        if resp.status().as_u16() == 429 {
            return Err(ReviewError::RateLimited429);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ReviewError::ProviderError(format!("HTTP {}: {}", status, body)));
        }

        let json_body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ReviewError::InvalidResponse(e.to_string()))?;

        let content = json_body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| ReviewError::InvalidResponse("Missing choices content".into()))?;

        let verdict: ReviewVerdict = serde_json::from_str(content)
            .map_err(|e| ReviewError::InvalidResponse(format!("JSON parse error: {}", e)))?;

        let in_tokens = json_body["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let out_tokens = json_body["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;
        let actual_cost = (in_tokens as f64 / 1_000_000.0) * 0.15 + (out_tokens as f64 / 1_000_000.0) * 0.60;

        Ok(ReviewResult {
            verdict,
            cached: false,
            dedup_hash: dedup_hash.to_string(),
            input_tokens: in_tokens,
            output_tokens: out_tokens,
            cost_usd: actual_cost,
            provider: format!("openai:{}", self.config.model),
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
        let has_tests = req.test_manifest.contains("test result: ok")
            || req.test_manifest.contains("PASSED");

        ReviewResult {
            verdict: ReviewVerdict {
                // Cannot falsely certify passed or requirements_covered without external review
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
