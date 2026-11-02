use std::path::PathBuf;
use std::sync::Arc;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use sail_forge::{
    ForgeNightlyRunner, ForgeOrchestrator, GhCliClient, LocalModelReviewer, NightlyConfig,
    OrchestratorConfig, ReviewBroker, ReviewPolicyConfig, TaskCheckpointStore,
};
use sail_guardian::sandbox::SandboxPolicy;

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") || !args.iter().any(|a| a == "--once") {
        eprintln!(
            "Usage: sail_forge_nightly --once [--dry-run] [--repo-root <path>]\n\n\
             Runs one Forge nightly pass now (ignoring the time window).\n\
             --dry-run only fetches and admits issues; it writes nothing and never pushes.\n\
             Limits and the kill-switch come from SAIL_FORGE_* environment variables."
        );
        std::process::exit(if args.iter().any(|a| a == "--once") {
            0
        } else {
            1
        });
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let repo_root = args
        .iter()
        .position(|a| a == "--repo-root")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let mut config = OrchestratorConfig::default();
    if let Some(root) = repo_root {
        config.workspace_root = root;
    }
    let orchestrator = Arc::new(
        ForgeOrchestrator::new(
            config,
            Arc::new(TaskCheckpointStore::default_store()),
            Arc::new(ReviewBroker::new(ReviewPolicyConfig::default())),
            Arc::new(SandboxPolicy::new()),
        )
        .with_self_reviewer(Arc::new(LocalModelReviewer::new())),
    );
    let runner = ForgeNightlyRunner::new(
        orchestrator,
        Arc::new(GhCliClient::new()),
        NightlyConfig::from_env(),
    );

    let digest = runner.run_once(dry_run, false).await;
    println!("{}", digest.to_markdown());
    if digest.halted.is_some() {
        std::process::exit(2);
    }
}
