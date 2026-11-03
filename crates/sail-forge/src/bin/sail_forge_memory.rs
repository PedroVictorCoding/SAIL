//! Inspect and correct Forge's failure memory (#121).
//!
//! ```text
//! sail_forge_memory list [--issue owner/repo#N]
//! sail_forge_memory export            # JSON array on stdout
//! sail_forge_memory invalidate <record-id | owner/repo#N> <reason...>
//! sail_forge_memory delete <record-id | owner/repo#N>
//! ```
//!
//! The store is `$SAIL_FORGE_STATE_DIR/failure_memory.jsonl`. Deleting records never
//! touches Git history; it only removes what Forge remembers about past attempts.

use sail_forge::failure_memory::{AttemptOutcome, FailureMemory};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mem = FailureMemory::open_default();
    let result = match args.first().map(String::as_str) {
        Some("list") => list(
            &mem,
            args.get(1).filter(|a| *a == "--issue").and(args.get(2)),
        ),
        Some("export") => mem.load().map(|r| {
            println!(
                "{}",
                serde_json::to_string_pretty(&r).unwrap_or_else(|_| "[]".into())
            );
        }),
        Some("invalidate") if args.len() >= 3 => mem
            .invalidate(&args[1], &args[2..].join(" "))
            .map(|n| println!("invalidated {n} record(s)")),
        Some("delete") if args.len() == 2 => mem
            .delete(&args[1])
            .map(|n| println!("deleted {n} record(s)")),
        _ => {
            eprintln!(
                "usage: sail_forge_memory list [--issue owner/repo#N] | export | \
                 invalidate <id|issue> <reason> | delete <id|issue>\nstore: {}",
                mem.path().display()
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("failure memory at {}: {e}", mem.path().display());
        std::process::exit(1);
    }
}

fn list(mem: &FailureMemory, issue: Option<&String>) -> std::io::Result<()> {
    let key = issue.map(|i| sail_forge::failure_memory::issue_key(i));
    for r in mem.load()? {
        if key.as_ref().is_some_and(|k| *k != r.issue) {
            continue;
        }
        let outcome = match &r.outcome {
            AttemptOutcome::Verified => "verified".to_string(),
            AttemptOutcome::Failed { class } => format!("failed: {}", class.label()),
        };
        let invalid = r
            .invalidated
            .as_ref()
            .map(|why| format!("  [invalidated: {why}]"))
            .unwrap_or_default();
        println!(
            "{}  {}  {}  {}{}\n    signatures: {}\n    files: {}\n    {}",
            r.id,
            r.recorded_at.format("%Y-%m-%d %H:%M"),
            r.issue,
            outcome,
            invalid,
            r.signatures.join(", "),
            r.files.join(", "),
            r.summary.as_deref().unwrap_or("")
        );
    }
    Ok(())
}
