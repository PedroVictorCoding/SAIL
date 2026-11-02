use std::path::PathBuf;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

use sail_forge::contracts::ForgeBudgetConfig;
use sail_forge::solver::LocalSolverController;
use sail_forge::worktree::IsolatedWorktree;
use sail_guardian::sandbox::SandboxPolicy;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args[1] == "--help" || args[1] == "-h" {
        eprintln!(
            "Usage: {} <repo_root> [issue_title] [issue_body] [base_commit]",
            args.first()
                .map(|s| s.as_str())
                .unwrap_or("sail-forge-solve")
        );
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    let repo_root = PathBuf::from(&args[1]);
    let title = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "Local Issue Solver Task".to_string());
    let body = args.get(3).cloned().unwrap_or_default();
    let base_commit = args.get(4).cloned().unwrap_or_else(|| "HEAD".to_string());

    info!(
        "Starting headless local issue solver for repo {:?} at base commit {}",
        repo_root, base_commit
    );

    let job_id = format!("cli-{}", uuid::Uuid::new_v4());
    let sandbox_policy = std::sync::Arc::new(SandboxPolicy::new());

    let worktree = IsolatedWorktree::create(&job_id, &repo_root, &base_commit, sandbox_policy)?;
    info!("Initialized isolated worktree at {:?}", worktree.root_dir);

    let budget = ForgeBudgetConfig::default();
    let controller = LocalSolverController::new(budget.max_iterations as u32, 180, None);

    let log_dir = std::env::temp_dir().join(format!("sail-forge-cli-{}", job_id));
    let outcome = controller
        .solve_issue(
            &worktree,
            &title,
            &body,
            &log_dir,
            || false,
            |prog| {
                println!(
                    "[PROGRESS] [Turn {}] {} - {}",
                    prog.turn,
                    prog.model_action.as_deref().unwrap_or("working"),
                    prog.message
                );
            },
        )
        .await;

    println!("\n=== SOLVER OUTCOME ===");
    println!("Success: {}", outcome.success);
    println!("Status: {}", outcome.verification_status);
    println!(
        "Model: {} ({})",
        outcome.model.model_id, outcome.model.endpoint
    );
    println!(
        "Turns: {}, Iterations: {}",
        outcome.turns, outcome.iterations
    );
    println!("Modified files: {:?}", outcome.modified_files);
    if let Some(reason) = outcome.failure_reason {
        println!("Failure Reason: {}", reason);
    }
    println!("\n--- FINAL DIFF ---");
    println!("{}", outcome.final_diff);

    if outcome.success {
        Ok(())
    } else {
        std::process::exit(2);
    }
}
