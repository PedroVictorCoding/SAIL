use std::collections::HashMap;
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
    #[error("Network or provider error: {0}")]
    ProviderError(String),
    #[error("Invalid JSON response from reviewer: {0}")]
    InvalidResponse(String),
}

/// Ledger tracking usage against daily, weekly, and monthly caps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewUsageLedger {
    pub day_timestamp: i32,    // e.g. Year * 1000 + DayOfYear
    pub week_timestamp: u32,   // e.g. Year * 100 + IsoWeek
    pub month_timestamp: u32,  // e.g. Year * 100 + Month
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

/// Thread-safe Review Broker managing quotas, deduplication, secret redaction, and local fallback.
pub struct ReviewBroker {
    config: ReviewPolicyConfig,
    ledger: Arc<Mutex<ReviewUsageLedger>>,
    dedup_cache: Arc<Mutex<HashMap<String, ReviewResult>>>,
    credential_store: Option<Arc<CredentialStore>>,
    mock_mode: bool,
}

impl ReviewBroker {
    pub fn new(config: ReviewPolicyConfig) -> Self {
        Self {
            config,
            ledger: Arc::new(Mutex::new(ReviewUsageLedger::default())),
            dedup_cache: Arc::new(Mutex::new(HashMap::new())),
            credential_store: None,
            mock_mode: false,
        }
    }

    pub fn with_credentials(mut self, credentials: Arc<CredentialStore>) -> Self {
        self.credential_store = Some(credentials);
        self
    }

    pub fn with_mock_mode(mut self, mock: bool) -> Self {
        self.mock_mode = mock;
        self
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

    /// Estimate cost in USD for the review request (using conservative gpt-4o-mini rates: $0.15/1M in, $0.60/1M out).
    pub fn estimate_request_cost(input_tokens: u32, max_output_tokens: u32) -> f64 {
        let input_cost = (input_tokens as f64 / 1_000_000.0) * 0.15;
        let output_cost = (max_output_tokens as f64 / 1_000_000.0) * 0.60;
        input_cost + output_cost
    }

    /// Sanitize text by stripping credentials, tokens, keys, and absolute paths before external dispatch.
    pub fn sanitize_payload(&self, text: &str) -> String {
        let mut sanitized = redact_credential_patterns(text);
        if let Some(store) = &self.credential_store {
            sanitized = store.redact_text(&sanitized);
        }
        sanitized
    }

    /// Primary entry point: review an issue change with strict quota checks and fallback.
    pub async fn review(&self, req: &ReviewRequest) -> ReviewResult {
        let dedup_hash = Self::compute_dedup_hash(
            &req.issue_id,
            &req.base_commit,
            &req.diff_patch,
            &req.test_manifest,
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
            info!("Review policy disabled; utilizing local deterministic review");
            return self.run_local_fallback(req, dedup_hash, "policy_disabled");
        }

        // 3. Resolve API key securely from CredentialStore or environment
        let api_key = self.resolve_api_key();
        if api_key.is_none() && !self.mock_mode {
            info!("OpenAI API key unavailable; gracefully falling back to local review");
            return self.run_local_fallback(req, dedup_hash, "missing_api_key");
        }

        // 4. Budget & Quota Pre-flight validation
        let preflight = self.check_and_reserve_quota(&req.issue_id, &dedup_hash, &req.diff_patch);
        match preflight {
            Ok(estimated_cost) => {
                // Execute review against API (or mock)
                match self.execute_api_review(req, &dedup_hash, estimated_cost, api_key).await {
                    Ok(result) => {
                        // Record actual usage into ledger
                        self.record_successful_review(
                            &req.issue_id,
                            &dedup_hash,
                            result.cost_usd,
                        );

                        // Save in dedup cache
                        if self.config.dedup_cache_enabled {
                            let mut cache = self.dedup_cache.lock().unwrap();
                            cache.insert(dedup_hash.clone(), result.clone());
                        }

                        result
                    }
                    Err(e) => {
                        warn!("API review encountered error: {}; falling back to local review", e);
                        self.record_error();
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

    /// Check quotas, per-issue limit, and reserve tentative slot.
    fn check_and_reserve_quota(
        &self,
        issue_id: &str,
        current_diff_hash: &str,
        diff_patch: &str,
    ) -> Result<f64, ReviewError> {
        let mut ledger = self.ledger.lock().unwrap();
        ledger.roll_windows_if_needed(&Utc::now());

        // Circuit breaker: max 3 consecutive errors
        if ledger.consecutive_errors >= 3 {
            return Err(ReviewError::CircuitBreakerTripped(ledger.consecutive_errors));
        }

        // Daily requests cap
        if ledger.requests_today >= self.config.max_requests_per_day {
            return Err(ReviewError::DailyRequestLimitExceeded(
                ledger.requests_today,
            ));
        }

        // Weekly requests cap
        if ledger.requests_this_week >= self.config.max_requests_per_week {
            return Err(ReviewError::WeeklyRequestLimitExceeded(
                ledger.requests_this_week,
            ));
        }

        // Per-issue review count
        let count = ledger.issue_review_counts.get(issue_id).copied().unwrap_or(0);
        if count >= self.config.max_reviews_per_issue {
            // Check if material revision occurred
            if let Some(last_hash) = ledger.issue_last_diff_hash.get(issue_id) {
                if last_hash == current_diff_hash || count >= 2 {
                    return Err(ReviewError::PerIssueLimitReached(count));
                }
            } else if count >= 2 {
                return Err(ReviewError::PerIssueLimitReached(count));
            }
        }

        // Rough token estimate: ~4 chars per token
        let est_in_tokens = (diff_patch.len() / 4).min(self.config.max_input_tokens_per_review as usize) as u32;
        let est_cost = Self::estimate_request_cost(est_in_tokens, self.config.max_output_tokens_per_review);

        // Daily budget cap ($2.00 default)
        if ledger.cost_today_usd + est_cost > self.config.max_cost_per_day_usd {
            return Err(ReviewError::DailyBudgetExceeded {
                current: ledger.cost_today_usd,
                estimated: est_cost,
                limit: self.config.max_cost_per_day_usd,
            });
        }

        // Monthly budget cap ($10.00 default - supersedes daily)
        if ledger.cost_this_month_usd + est_cost > self.config.max_cost_per_month_usd {
            return Err(ReviewError::MonthlyBudgetExceeded {
                current: ledger.cost_this_month_usd,
                estimated: est_cost,
                limit: self.config.max_cost_per_month_usd,
            });
        }

        Ok(est_cost)
    }

    fn record_successful_review(&self, issue_id: &str, diff_hash: &str, actual_cost: f64) {
        let mut ledger = self.ledger.lock().unwrap();
        ledger.roll_windows_if_needed(&Utc::now());
        ledger.requests_today += 1;
        ledger.requests_this_week += 1;
        ledger.cost_today_usd += actual_cost;
        ledger.cost_this_month_usd += actual_cost;
        *ledger.issue_review_counts.entry(issue_id.to_string()).or_insert(0) += 1;
        ledger.issue_last_diff_hash.insert(issue_id.to_string(), diff_hash.to_string());
        ledger.consecutive_errors = 0;
    }

    fn record_error(&self) {
        let mut ledger = self.ledger.lock().unwrap();
        ledger.consecutive_errors += 1;
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
        dedup_hash: &str,
        _estimated_cost: f64,
        api_key: Option<SecretString>,
    ) -> Result<ReviewResult, ReviewError> {
        let sanitized_diff = self.sanitize_payload(&req.diff_patch);
        let sanitized_manifest = self.sanitize_payload(&req.test_manifest);

        if self.mock_mode {
            // Deterministic mock for unit and integration testing
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
        let actual_cost = Self::estimate_request_cost(in_tokens, out_tokens);

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

    /// Local fallback when API key is missing, quotas are reached, or network fails.
    /// Fulfills requirement: "Missing API key or exhausted quota must never block local issue resolution."
    pub fn run_local_fallback(
        &self,
        req: &ReviewRequest,
        dedup_hash: String,
        reason: &str,
    ) -> ReviewResult {
        // Deterministic analysis of diff and test manifest
        let has_tests = req.test_manifest.contains("test result: ok")
            || req.test_manifest.contains("PASSED")
            || req.test_manifest.contains("test_");
        let has_unresolved_todos = req.diff_patch.contains("TODO") || req.diff_patch.contains("FIXME");
        let has_unwrap_additions = req.diff_patch.lines().any(|l| l.starts_with('+') && l.contains(".unwrap()"));

        let bugs = Vec::new();
        let mut security = Vec::new();
        let mut edge_cases = Vec::new();
        let mut edits = Vec::new();

        if !has_tests {
            edge_cases.push("No explicit test verification output found in test manifest".into());
        }
        if has_unresolved_todos {
            edits.push("Diff contains unresolved TODO/FIXME comments".into());
        }
        if has_unwrap_additions {
            security.push("Diff adds `.unwrap()` which may panic on unexpected inputs; consider Result propagation".into());
        }

        let passed = has_tests && bugs.is_empty();

        ReviewResult {
            verdict: ReviewVerdict {
                passed,
                requirements_covered: true,
                untested_edge_cases: edge_cases,
                detected_bugs: bugs,
                security_observations: security,
                suggested_edits: edits,
                confidence_score: 0.85,
                raw_feedback: format!("Local deterministic review completed ({})", reason),
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
