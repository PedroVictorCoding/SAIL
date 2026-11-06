//! Runs the model eval (`sail_dialogue::model_eval`) against a llama-server endpoint and
//! writes `<out>/<label>.json` and `.md`. See docs/MODEL_EVAL.md.
use sail_dialogue::model_eval::{
    ChatEndpoint, EvalReport, grounded_cases, judge_cases, markdown, run_grounded_case,
    run_judge_case, run_tool_case, server_memory, summarize, tool_cases,
};
use std::path::PathBuf;

fn arg<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

const USAGE: &str = "\
Usage: sail_model_eval [--url <base>] [--label <name>] [--suites tools,judge,grounded]
                       [--only <id,id>] [--repeat <n>] [--out <dir>] [--compare <baseline.json>]

Scores the model behind a llama-server endpoint on SAIL's own work: voice tool calls,
the Arena Learn judge, and answers grounded in a memory block. Uses the production
prompts and tool schemas. --url defaults to SAIL_LLM_URL, else http://127.0.0.1:8081.
--compare applies the adoption gate (no suite more than 5 points below the baseline).
Writes <out>/<label>.json and <out>/<label>.md; --out defaults to .local/model-eval.";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    let url = arg(&args, "--url")
        .map(str::to_string)
        .or_else(|| std::env::var("SAIL_LLM_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8081".into());
    let suites: Vec<&str> = arg(&args, "--suites")
        .unwrap_or("tools,judge,grounded")
        .split(',')
        .map(str::trim)
        .collect();
    let only: Option<Vec<&str>> =
        arg(&args, "--only").map(|v| v.split(',').map(str::trim).collect());
    let keep = |id: &str| only.as_ref().is_none_or(|ids| ids.contains(&id));
    let repeat: usize = arg(&args, "--repeat")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let out = PathBuf::from(arg(&args, "--out").unwrap_or(".local/model-eval"));
    let baseline: Option<EvalReport> = match arg(&args, "--compare") {
        Some(path) => match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        {
            Ok(report) => Some(report),
            Err(e) => {
                eprintln!("cannot read baseline {path}: {e}");
                std::process::exit(2);
            }
        },
        None => None,
    };

    let endpoint = ChatEndpoint::new(&url);
    let model = match endpoint.model_id().await {
        Ok(model) => model,
        Err(e) => {
            eprintln!("model server unavailable: {e}");
            std::process::exit(2);
        }
    };
    let label = arg(&args, "--label")
        .map(str::to_string)
        .unwrap_or_else(|| {
            let stem = model
                .rsplit('/')
                .next()
                .unwrap_or(&model)
                .trim_end_matches(".gguf");
            format!("{stem}-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"))
        });
    eprintln!("eval {label}: model {model} at {url}");

    let mut report = EvalReport {
        label: label.clone(),
        model,
        endpoint: url,
        started_at: chrono::Utc::now().to_rfc3339(),
        repeat,
        scores: Vec::new(),
        median_prompt_per_second: 0.0,
        median_generated_per_second: 0.0,
        p50_wall_ms: 0,
        p95_wall_ms: 0,
        server_memory: Vec::new(),
        cases: Vec::new(),
    };
    for _ in 0..repeat {
        if suites.contains(&"tools") {
            for case in tool_cases().iter().filter(|c| keep(c.id)) {
                let r = run_tool_case(&endpoint, case).await;
                log(&r);
                report.cases.push(r);
            }
        }
        if suites.contains(&"judge") {
            for case in judge_cases().iter().filter(|c| keep(c.id)) {
                let r = run_judge_case(&endpoint, case).await;
                log(&r);
                report.cases.push(r);
            }
        }
        if suites.contains(&"grounded") {
            for case in grounded_cases().iter().filter(|c| keep(c.id)) {
                let r = run_grounded_case(&endpoint, case).await;
                log(&r);
                report.cases.push(r);
            }
        }
    }
    report.server_memory = server_memory();
    summarize(&mut report);

    let md = markdown(&report, baseline.as_ref());
    if let Err(e) = std::fs::create_dir_all(&out)
        .and_then(|_| {
            std::fs::write(
                out.join(format!("{label}.json")),
                serde_json::to_string_pretty(&report).unwrap(),
            )
        })
        .and_then(|_| std::fs::write(out.join(format!("{label}.md")), &md))
    {
        eprintln!("cannot write report to {}: {e}", out.display());
        std::process::exit(2);
    }
    println!("{md}");
    eprintln!("wrote {}", out.join(format!("{label}.json")).display());
}

fn log(r: &sail_dialogue::model_eval::CaseResult) {
    let ms: u64 = r.timings.iter().map(|t| t.wall_ms).sum();
    eprintln!(
        "  {} {:<9} {:<18} {:>6} ms  {}",
        if r.passed { "PASS" } else { "FAIL" },
        r.suite,
        r.id,
        ms,
        r.detail
    );
}
