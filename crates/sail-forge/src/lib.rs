#![allow(clippy::type_complexity, clippy::collapsible_if, clippy::manual_strip)]

pub mod admission;
pub mod contracts;
pub mod edit;
pub mod eval;
pub mod eval_cases;
pub mod fixtures;
pub mod gates;
pub mod github;
pub mod model_bench;
pub mod mutation;
pub mod nightly;
pub mod orchestrator;
pub mod persistence;
pub mod publish;
pub mod repo_map;
pub mod reviewer;
pub mod self_review;
pub mod solver;
pub mod subsystem;
pub mod test_report;
pub mod worktree;

pub use admission::{AdmissionDecision, AdmissionPolicy, IssueCandidate, parse_target_files};
pub use contracts::*;
pub use fixtures::{IssueFixture, get_benchmark_fixtures};
pub use gates::{DiffGatePolicy, GateViolation};
pub use github::{DraftPr, GhCliClient, GithubClient, OpenPullRequest};
pub use model_bench::{
    BenchmarkHarness, FixtureBenchmarkResult, ModelBenchmarkMetrics, ModelCandidateConfig,
};
pub use nightly::{ForgeNightlyRunner, JobExecutor, NightlyConfig, NightlyDigest, NightlyState};
pub use orchestrator::{
    ActiveJob, ForgeOrchestrator, OrchestratorConfig, clear_global_orchestrator,
    global_orchestrator, register_global_orchestrator,
};
pub use persistence::{TaskCheckpoint, TaskCheckpointStore};
pub use publish::{PublishConfig, publish_draft_pr};
pub use reviewer::{ReviewBroker, ReviewError, ReviewUsageLedger};
pub use self_review::{LocalModelReviewer, ReviewPacket, ReviewProvider, parse_verdict};
pub use solver::{
    IdentifiedModel, LocalSolverController, RepoToolCall, RepoToolResult, SolverOutcome,
    execute_repo_tool, identify_local_model, is_meaningful_code_diff,
};
pub use subsystem::ForgeSubsystem;
pub use worktree::{IsolatedWorktree, WorktreeError};
