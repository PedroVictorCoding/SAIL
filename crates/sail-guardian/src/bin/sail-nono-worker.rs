//! One-shot Linux enforcement worker. The trusted supervisor supplies authorization
//! over stdin; this executable is not an authorization or remote request endpoint.
use std::io::{self, Read};
use std::path::PathBuf;

use sail_guardian::runtime::{AuthorizedAction, NonoSecurityRuntime, SecurityRuntime};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    action: AuthorizedAction,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("sail-nono-worker refused execution: {error}");
        std::process::exit(125);
    }
}

#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("this worker requires Linux enforcement".into())
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::process::CommandExt;

    // This process is freshly exec'd and single-threaded. Never apply irreversible
    // Landlock restrictions in the long-lived supervisor or in pre_exec().
    const MAX_REQUEST: u64 = 64 * 1024;
    let mut input = Vec::new();
    io::stdin().take(MAX_REQUEST + 1).read_to_end(&mut input)?;
    if input.len() as u64 > MAX_REQUEST {
        return Err("request exceeds 64 KiB".into());
    }
    let request: Request = serde_json::from_slice(&input)?;
    if request.action.expires_at <= chrono::Utc::now() {
        return Err("authorization expired".into());
    }
    if !request.program.is_absolute() || !request.cwd.is_absolute() {
        return Err("program and cwd must be absolute".into());
    }
    if matches!(
        request.action.network_mode,
        nono::NetworkMode::ProxyOnly { .. }
    ) {
        return Err("proxy-only networking requires a supervised proxy filter".into());
    }
    let null_stdin = std::fs::File::open("/dev/null")?;
    let runtime = NonoSecurityRuntime::new();
    let caps = runtime.build_capability_set(&request.action)?;
    if caps.resource_limits().is_some() {
        return Err(
            "resource limits require the dedicated runner; this worker cannot enforce quotas"
                .into(),
        );
    }
    // Any application failure terminates this worker, without running the target.
    let _enforcement = nono::Sandbox::apply_auto(&caps)?;
    std::env::set_current_dir(&request.cwd)?;
    // Consume and close the authorization channel before handing control over.
    let error = std::process::Command::new(&request.program)
        .args(&request.args)
        .env_clear()
        .stdin(std::process::Stdio::from(null_stdin))
        .exec();
    Err(error.into())
}
