use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle state for durable `forge:issue_solve` tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeTaskState {
    Queued,
    Planning,
    Coding,
    Testing,
    ReviewPending,
    AwaitingUser,
    Completed,
    Blocked,
}

impl std::fmt::Display for ForgeTaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Queued => "queued",
                Self::Planning => "planning",
                Self::Coding => "coding",
                Self::Testing => "testing",
                Self::ReviewPending => "review_pending",
                Self::AwaitingUser => "awaiting_user",
                Self::Completed => "completed",
                Self::Blocked => "blocked",
            }
        )
    }
}

/// Resource budget limits for a single Forge issue task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeBudgetConfig {
    pub max_wall_clock_secs: u64,
    pub max_model_tokens: u64,
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    pub max_disk_bytes: u64,
    pub max_compilation_cpu_secs: u64,
    pub max_review_credits: usize,
}

impl Default for ForgeBudgetConfig {
    fn default() -> Self {
        Self {
            max_wall_clock_secs: 14_400, // 4 hours
            max_model_tokens: 250_000,
            max_iterations: 15,
            max_tool_calls: 100,
            max_disk_bytes: 500 * 1024 * 1024, // 500MB
            max_compilation_cpu_secs: 1_800,   // 30 min compile time
            max_review_credits: 2,
        }
    }
}

/// Strict rate and cost caps for optional external OpenAI API reviewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPolicyConfig {
    pub enabled: bool,
    pub max_reviews_per_issue: usize,
    pub max_requests_per_day: usize,
    pub max_requests_per_week: usize,
    pub max_cost_per_day_usd: f64,
    pub max_cost_per_month_usd: f64,
    pub max_input_tokens_per_review: u32,
    pub max_output_tokens_per_review: u32,
    pub model: String,
    pub dedup_cache_enabled: bool,
}

impl Default for ReviewPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_reviews_per_issue: 1, // max 2 only after material revision
            max_requests_per_day: 3,
            max_requests_per_week: 10,
            max_cost_per_day_usd: 2.00,
            max_cost_per_month_usd: 10.00,
            max_input_tokens_per_review: 20_000,
            max_output_tokens_per_review: 4_000,
            model: "gpt-4o-mini".into(),
            dedup_cache_enabled: true,
        }
    }
}

/// Request to enqueue an issue for autonomous resolution (`forge:issue_enqueue`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueEnqueueRequest {
    pub repo: String,
    pub issue_number: u64,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub review_policy: Option<ReviewPolicyConfig>,
    #[serde(default)]
    pub budget: Option<ForgeBudgetConfig>,
}

/// Formulated plan for resolving an issue (`forge:issue_plan`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuePlan {
    pub job_id: String,
    pub repo: String,
    pub issue_number: u64,
    pub title: String,
    pub summary: String,
    pub steps: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub estimated_budget: ForgeBudgetConfig,
}

/// Event record in a task's timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeTaskEvent {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub stage: String,
    pub detail: String,
    pub idempotency_key: String,
}

/// Real-time status of a Forge issue job (`forge:issue_status`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueStatusResponse {
    pub job_id: String,
    pub repo: String,
    pub issue_number: u64,
    pub state: ForgeTaskState,
    pub branch: String,
    pub current_step: String,
    pub iteration: usize,
    pub diff_stat: String,
    pub tokens_used: u64,
    pub wall_time_secs: u64,
    pub review_status: Option<String>,
    pub events: Vec<ForgeTaskEvent>,
}

/// Request to pause an active task (`forge:issue_pause`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuePauseRequest {
    pub job_id: String,
    pub reason: String,
}

/// Request to resume a paused task (`forge:issue_resume`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueResumeRequest {
    pub job_id: String,
}

/// Request to cancel a task (`forge:issue_cancel`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueCancelRequest {
    pub job_id: String,
    pub reason: String,
}

/// Request for a job report (`forge:issue_report`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueReportRequest {
    pub job_id: String,
}

/// Input to the review broker (`forge:review_request`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub job_id: String,
    pub issue_id: String,
    pub base_commit: String,
    pub diff_patch: String,
    pub test_manifest: String,
}

/// Parsed verdict returned by reviewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewVerdict {
    pub passed: bool,
    pub requirements_covered: bool,
    pub untested_edge_cases: Vec<String>,
    pub detected_bugs: Vec<String>,
    pub security_observations: Vec<String>,
    pub suggested_edits: Vec<String>,
    pub confidence_score: f32,
    pub raw_feedback: String,
}

/// Overall review execution result including budget and cache provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResult {
    pub verdict: ReviewVerdict,
    pub cached: bool,
    pub dedup_hash: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
    pub provider: String,
}

/// Request to prepare draft PR artifacts (`forge:pr_draft`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrDraftRequest {
    pub job_id: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// Output of PR draft preparation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrDraftResult {
    pub branch: String,
    pub base_branch: String,
    pub title: String,
    pub body: String,
    pub diff_stat: String,
    pub artifacts: Vec<String>,
    pub policy_approved: bool,
    pub pr_url: Option<String>,
}

/// Record of an executed test run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestExecutionRecord {
    pub command: String,
    pub status: String,
    pub duration_ms: u64,
    pub log_ref: String,
}

/// Comprehensive report for an issue resolution attempt (`ForgeJobReport`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeJobReport {
    pub issue: String,
    pub base_commit: String,
    pub branch: String,
    pub model_id: String,
    pub model_hash: String,
    pub patch_ref: String,
    pub tests: Vec<TestExecutionRecord>,
    pub review_result: Option<ReviewResult>,
    pub cost_local_estimate: f64,
    pub api_cost_actual: f64,
    pub remaining_risks: Vec<String>,
    pub status: ForgeTaskState,
}
