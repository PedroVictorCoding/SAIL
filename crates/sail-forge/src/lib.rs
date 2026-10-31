#![allow(clippy::type_complexity, clippy::collapsible_if, clippy::manual_strip)]

pub mod contracts;
pub mod fixtures;
pub mod model_bench;
pub mod orchestrator;
pub mod persistence;
pub mod reviewer;
pub mod subsystem;
pub mod worktree;

pub use contracts::*;
pub use fixtures::{IssueFixture, get_benchmark_fixtures};
pub use model_bench::{
    BenchmarkHarness, FixtureBenchmarkResult, ModelBenchmarkMetrics, ModelCandidateConfig,
};
pub use orchestrator::{
    ActiveJob, ForgeOrchestrator, OrchestratorConfig, clear_global_orchestrator,
    global_orchestrator, register_global_orchestrator,
};
pub use persistence::{TaskCheckpoint, TaskCheckpointStore};
pub use reviewer::{ReviewBroker, ReviewError, ReviewUsageLedger};
pub use subsystem::ForgeSubsystem;
pub use worktree::{IsolatedWorktree, WorktreeError};
