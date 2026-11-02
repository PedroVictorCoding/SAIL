use std::path::PathBuf;
use std::time::Duration;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

use sail_forge::eval::{EvalRun, builtin_cases, run_case, wait_for_model};
use sail_forge::solver::{LocalSolverController, identify_local_model};

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!(
            "Usage: sail_forge_eval [--label <name>] [--only <id,id>] [--max-iterations <n>]\n\
             \x20                      [--out <dir>] [--compare <previous.json>]\n\n\
             Runs the real local solver on the validated eval cases and grades each with\n\
             tests the solver never sees. Writes <out>/<label>.json and .md."
        );
        return;
    }

    let label = arg_value(&args, "--label")
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let max_iterations: u32 = arg_value(&args, "--max-iterations")
        .and_then(|v| v.parse().ok())
        .unwrap_or(sail_forge::ForgeBudgetConfig::default().max_iterations as u32);
    let only: Option<Vec<u64>> = arg_value(&args, "--only")
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect());
    let out_dir = arg_value(&args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var("SAIL_FORGE_STATE_DIR")
                .map(PathBuf::from)
                .or_else(|_| {
                    std::env::var("HOME").map(|h| PathBuf::from(h).join(".local/state/sail/forge"))
                })
                .unwrap_or_else(|_| std::env::temp_dir().join("sail_forge_state"))
                .join("eval")
        });

    let endpoint = LocalSolverController::candidate_endpoints()
        .into_iter()
        .next()
        .unwrap_or_default();
    let model = identify_local_model(&reqwest::Client::new(), &endpoint, None).await;
    let controller = LocalSolverController::new(max_iterations, 300, None);
    let scratch = std::env::temp_dir().join(format!("sail_forge_eval_{}", uuid::Uuid::new_v4()));

    let mut run = EvalRun {
        started_at: chrono::Utc::now(),
        label: label.clone(),
        model: model.model_id.clone(),
        endpoint: endpoint.clone(),
        max_iterations,
        cases: Vec::new(),
    };

    for case in builtin_cases() {
        if only.as_ref().is_some_and(|ids| !ids.contains(&case.id)) {
            continue;
        }
        info!("eval case {}: {}", case.id, case.title);
        let mut result = run_case(&case, &controller, &scratch).await;
        if result.unavailable {
            // Ride out a busy or restarting server once before giving up on the run.
            info!(
                "eval case {}: model unavailable ({}); waiting up to 15 min to retry",
                case.id, result.detail
            );
            if wait_for_model(&endpoint, Duration::from_secs(900), Duration::from_secs(15)).await {
                result = run_case(&case, &controller, &scratch).await;
            }
        }
        info!(
            "eval case {} -> solved={} claimed={} edit_failures={} turns={} ({}s)",
            result.id,
            result.solved,
            result.solver_claimed_success,
            result.edit_failures,
            result.turns,
            result.wall_ms / 1000
        );
        let unavailable = result.unavailable;
        run.cases.push(result);
        // Without a model nothing further is measurable; stop instead of faking results.
        if unavailable {
            break;
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);

    let mut md = run.to_markdown();
    if let Some(prev) = arg_value(&args, "--compare") {
        match std::fs::read_to_string(prev)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<EvalRun>(&s).map_err(|e| e.to_string()))
        {
            Ok(baseline) => md.push_str(&format!("\n{}", run.compare(&baseline))),
            Err(e) => eprintln!("could not read baseline {prev}: {e}"),
        }
    }

    if std::fs::create_dir_all(&out_dir).is_ok() {
        if let Ok(json) = serde_json::to_string_pretty(&run) {
            let _ = std::fs::write(out_dir.join(format!("{label}.json")), json);
        }
        let _ = std::fs::write(out_dir.join(format!("{label}.md")), &md);
    }
    println!("{md}");
    if run.measured_count() == 0 {
        std::process::exit(2);
    }
}
