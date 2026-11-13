//! Vendor CLI providers (#449): Claude Code, Codex and Gemini CLI behind [`ExternalProvider`].
//!
//! These use the owner's signed-in subscriptions instead of API keys (owner decision 12).
//! A CLI is an agent with tools; SAIL uses it only as a text-in, text-out model.
//!
//! # Contract (Kilo implements the `unimplemented!()` bodies)
//!
//! **Configuration.** `ProviderConfig { adapter: "claude_cli" | "codex_cli" | "gemini_cli",
//! model, command, cli_unverified_ok, .. }`. `base_url` and `api_key_env` are ignored.
//!
//! **`CliProvider::new`** resolves the executable: `config.command` (absolute path, or a
//! bare name looked up on `PATH`), else [`Vendor::default_binary`]. Not found or not
//! executable → `ProviderError::Unavailable("… not installed")`. It does not run the CLI.
//!
//! **Version gate.** On the first `complete`, run `<bin> --version` (10 s timeout, same
//! sandbox as below) and keep the result for the provider's lifetime. Parse the first
//! `MAJOR.MINOR.PATCH` in the output. If it does not start with one of
//! [`Vendor::verified_versions`] and `config.cli_unverified_ok` is false →
//! `Unavailable("unverified <vendor> CLI version X; flags verified for …")`. A failing
//! `--version` → `Unavailable`.
//!
//! **Sandbox for every run.**
//! - Working directory: a fresh empty temp dir (`tempfile::TempDir`), removed afterwards.
//! - Environment cleared, then only: `PATH`, `HOME`, `USER`, `LANG`, `LC_ALL`, `TERM=dumb`,
//!   `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME` (when set in SAIL's environment),
//!   plus `NO_COLOR=1` and `CI=1`. Never pass SAIL's own variables (`SAIL_*`, tokens).
//!   The CLI's sign-in lives under `HOME`, which is why `HOME` is kept.
//! - stdin carries the prompt (see [`Vendor::args`]); it is never put on the command line,
//!   so it doesn't show in `ps`.
//! - stdout and stderr are each read up to [`MAX_OUTPUT_BYTES`]; more is a failure
//!   (`Http("output too large")`), not a truncated answer.
//! - Timeout [`RUN_TIMEOUT`]; `cancel` → kill the child (and its process group) and return
//!   `Cancelled`. Use `tokio::process::Command` with `kill_on_drop(true)` and
//!   `process_group(0)`.
//!
//! **Prompt.** `"{system}\n\n---\n\n{prompt}"` on stdin. The answer must be text only;
//! `max_tokens` is passed only where the CLI has a flag for it (none of the three today),
//! and otherwise enforced by SAIL's budget accounting after the run.
//!
//! **Result.** Exit 0 → [`Vendor::parse_output`] gives the reply. The answering model is the
//! one the CLI reports when it reports one, else `config.model`, else `"<vendor> default"`.
//! Token counts come from the CLI's usage report when present, else `chars / 4` of the
//! prompt and answer (so caps still count something). An empty answer is `Http("empty
//! answer")`.
//!
//! Non-zero exit → [`Vendor::classify_failure`]:
//! - rate-limit / quota / usage-limit text → `RateLimited { retry_after }` (parse a reset
//!   time or "try again in N minutes/hours" when present);
//! - not signed in / auth / login text → `Unavailable("<vendor> CLI is not signed in")`;
//! - anything else → `Http("<vendor> CLI exited with <code>: <first 300 chars of stderr>")`
//!   with control characters stripped.
//!   Output never becomes a fake answer.
//!
//! **Permission flags.** [`Vendor::args`] pins flags that make each CLI non-interactive,
//! read-only and tool-free (no shell, no file edits, no web fetch, no MCP servers). The
//! drafts below come from Claude's knowledge, not from the installed CLIs (none are
//! installed on the SAIL host yet). Kilo checks each against the vendor's current docs and
//! cites the doc URL and date in a comment. `verified_versions` stays **empty** until a
//! real binary has been run on this host with those flags; until then, using a CLI needs
//! `cli_unverified_ok: true` in the owner's policy file.
//!
//! | Vendor | Draft argv (prompt on stdin) | Output |
//! |---|---|---|
//! | Claude Code | `-p --output-format json --tools "" --strict-mcp-config --permission-mode default --no-session-persistence [--model M]` | one JSON object: `result`, `is_error`, `usage.{input_tokens,output_tokens}`, `modelUsage` keys = model ids |
//! | Codex | `exec --json --sandbox read-only --skip-git-repo-check --ephemeral --color never [-m M] -` | JSONL events: last `item.completed` with `item.type == "agent_message"` → `item.text`; `turn.completed.usage` |
//! | Gemini | `--output-format json --approval-mode default [-m M]` (+ empty `--allowed-tools`/excluded tools if supported) | one JSON object: `response`, `stats.models.<id>.tokens.{prompt,candidates}`, `error` |
//!
//! Where a draft flag does not exist in the current CLI, use the documented equivalent and
//! note it in the Log. If a vendor has no way to turn tools off non-interactively, its
//! `args` must still keep the run read-only (sandbox flag) and the doc comment says so.
//!
//! **Tests** (`crates/sail-mind/tests/escalation_cli.rs`, no network, no real CLI): fake
//! CLIs are small `#!/bin/sh` scripts written to a temp dir and passed as `command`, with
//! `cli_unverified_ok: true` except in the version-gate tests. Cover per vendor: success
//! (answer, model and tokens parsed), rate limit → `RateLimited` (with and without a reset
//! time), not signed in → `Unavailable`, other failure → `Http`, missing binary →
//! `Unavailable` from `new`, unverified version → `Unavailable`, the prompt arrives on
//! stdin and not in argv, the environment has no `SAIL_*` variable and the working
//! directory is empty, output over the cap → error, cancel kills a sleeping script within
//! 2 s, and `provider::build` returns a `CliProvider` for each adapter name.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use super::ProviderConfig;
use super::provider::{ExternalProvider, ProviderError, ProviderReply};

/// Per stream (stdout, stderr).
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// One CLI run, end to end.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(180);
/// `--version`.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Environment variables a CLI legitimately needs. Everything else is dropped.
const KEPT_ENV: [&str; 8] = [
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    ClaudeCode,
    Codex,
    Gemini,
}

/// What [`Vendor::parse_output`] extracts from a successful run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliAnswer {
    pub text: String,
    /// The model the CLI says answered, when it says.
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl Vendor {
    /// `claude_cli` → ClaudeCode, `codex_cli` → Codex, `gemini_cli` → Gemini.
    pub fn from_adapter(adapter: &str) -> Option<Self> {
        match adapter {
            "claude_cli" => Some(Self::ClaudeCode),
            "codex_cli" => Some(Self::Codex),
            "gemini_cli" => Some(Self::Gemini),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini",
        }
    }

    pub fn default_binary(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
        }
    }

    /// Version prefixes (`"2.1."`) whose flags were run on this host. Empty until verified.
    pub fn verified_versions(self) -> &'static [&'static str] {
        &[]
    }

    /// The pinned argv after the binary. `model` empty → no model flag.
    pub fn args(self, model: &str) -> Vec<String> {
        let mut args: Vec<String> = match self {
            // https://code.claude.com/docs/en/cli-reference — checked 2026-11-13.
            // `-p` is non-interactive, `--tools ""` removes every built-in tool,
            // `--strict-mcp-config` with no `--mcp-config` loads no MCP servers, and
            // `--permission-mode default` never auto-approves anything.
            Self::ClaudeCode => [
                "-p",
                "--output-format",
                "json",
                "--tools",
                "",
                "--strict-mcp-config",
                "--permission-mode",
                "default",
                "--no-session-persistence",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            // https://developers.openai.com/codex/developer-commands — checked 2026-11-13.
            // `exec` is the non-interactive command, `--json` gives JSONL events,
            // `--sandbox read-only` keeps generated commands from writing, and the trailing
            // `-` reads the prompt from stdin.
            Self::Codex => [
                "exec",
                "--json",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--ephemeral",
                "--color",
                "never",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            // https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/cli-reference.md
            // — checked 2026-11-13. `--approval-mode default` never auto-approves tools and
            // `--sandbox` confines whatever the agent tries to run. The current docs mark
            // `--allowed-tools` deprecated and list no non-interactive way to remove tools
            // entirely, so the sandbox is the tool-free guarantee here.
            Self::Gemini => [
                "--output-format",
                "json",
                "--approval-mode",
                "default",
                "--sandbox",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        };
        if !model.trim().is_empty() {
            match self {
                Self::ClaudeCode => {
                    args.push("--model".to_string());
                    args.push(model.to_string());
                }
                Self::Codex => {
                    args.push("-m".to_string());
                    args.push(model.to_string());
                }
                Self::Gemini => {
                    args.push("-m".to_string());
                    args.push(model.to_string());
                }
            }
        }
        if self == Self::Codex {
            args.push("-".to_string());
        }
        args
    }

    /// Parse stdout of a run that exited 0.
    pub fn parse_output(self, stdout: &str) -> Result<CliAnswer, ProviderError> {
        match self {
            Self::ClaudeCode => parse_claude(stdout),
            Self::Codex => parse_codex(stdout),
            Self::Gemini => parse_gemini(stdout),
        }
    }

    /// Map a non-zero exit to an error (rate limit, not signed in, other).
    pub fn classify_failure(
        self,
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
        now: DateTime<Utc>,
    ) -> ProviderError {
        let combined = format!("{stderr}\n{stdout}").to_lowercase();
        if mentions_rate_limit(&combined) {
            return ProviderError::RateLimited {
                retry_after: parse_reset(&combined, now),
            };
        }
        if mentions_auth(&combined) {
            return ProviderError::Unavailable(format!("{} CLI is not signed in", self.label()));
        }
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        let code = exit_code.map_or_else(|| "signal".to_string(), |code| code.to_string());
        ProviderError::Http(format!(
            "{} CLI exited with {code}: {}",
            self.label(),
            plain_text(detail)
        ))
    }
}

fn parse_claude(stdout: &str) -> Result<CliAnswer, ProviderError> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        ProviderError::Http(format!("Claude Code returned invalid JSON: {error}"))
    })?;
    if value.get("is_error").and_then(Value::as_bool) == Some(true) {
        let message = value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(ProviderError::Http(format!(
            "Claude Code reported an error: {}",
            plain_text(message)
        )));
    }
    let model = value
        .get("modelUsage")
        .and_then(Value::as_object)
        .and_then(|models| {
            models
                .iter()
                .max_by_key(|(_, usage)| {
                    usage
                        .get("outputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                })
                .map(|(model, _)| model.clone())
        });
    let usage = &value["usage"];
    Ok(CliAnswer {
        text: value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        model,
        input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
        output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
    })
}

fn parse_codex(stdout: &str) -> Result<CliAnswer, ProviderError> {
    let mut text = None;
    let mut input_tokens = None;
    let mut output_tokens = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("item.completed") => {
                let item = &event["item"];
                if item.get("type").and_then(Value::as_str) == Some("agent_message")
                    && let Some(value) = item.get("text").and_then(Value::as_str)
                {
                    text = Some(value.to_string());
                }
            }
            Some("turn.completed") => {
                let usage = &event["usage"];
                if let Some(value) = usage.get("input_tokens").and_then(Value::as_u64) {
                    input_tokens = Some(value);
                }
                if let Some(value) = usage.get("output_tokens").and_then(Value::as_u64) {
                    output_tokens = Some(value);
                }
            }
            _ => {}
        }
    }
    let text =
        text.ok_or_else(|| ProviderError::Http("Codex returned no agent message".to_string()))?;
    Ok(CliAnswer {
        text,
        model: None,
        input_tokens,
        output_tokens,
    })
}

fn parse_gemini(stdout: &str) -> Result<CliAnswer, ProviderError> {
    let value: Value = serde_json::from_str(stdout.trim())
        .map_err(|error| ProviderError::Http(format!("Gemini returned invalid JSON: {error}")))?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let message = error
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string());
        return Err(ProviderError::Http(format!(
            "Gemini reported an error: {}",
            plain_text(&message)
        )));
    }
    let mut input_tokens = None;
    let mut output_tokens = None;
    let mut model = None;
    let mut best = 0;
    if let Some(models) = value
        .get("stats")
        .and_then(|stats| stats.get("models"))
        .and_then(Value::as_object)
    {
        for (id, stats) in models {
            let tokens = stats.get("tokens");
            let prompt = tokens
                .and_then(|tokens| tokens.get("prompt"))
                .and_then(Value::as_u64);
            let candidates = tokens
                .and_then(|tokens| tokens.get("candidates"))
                .and_then(Value::as_u64);
            if prompt.is_some() || candidates.is_some() {
                input_tokens = Some(input_tokens.unwrap_or(0) + prompt.unwrap_or(0));
                output_tokens = Some(output_tokens.unwrap_or(0) + candidates.unwrap_or(0));
            }
            let candidates = candidates.unwrap_or(0);
            if model.is_none() || candidates >= best {
                best = candidates;
                model = Some(id.clone());
            }
        }
    }
    Ok(CliAnswer {
        text: value
            .get("response")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        model,
        input_tokens,
        output_tokens,
    })
}

fn mentions_rate_limit(text: &str) -> bool {
    [
        "rate limit",
        "rate_limit",
        "rate-limit",
        "ratelimit",
        "too many requests",
        "quota",
        "usage limit",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn mentions_auth(text: &str) -> bool {
    [
        "not signed in",
        "not logged in",
        "signed out",
        "login required",
        "log in to",
        "sign in to",
        "please log in",
        "please sign in",
        "signin",
        "unauthorized",
        "authentication failed",
        "invalid api key",
        "expired token",
        "auth required",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn parse_reset(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    for marker in ["try again in ", "retry after ", "retry in "] {
        if let Some(index) = text.find(marker)
            && let Some(wait) = parse_duration(&text[index + marker.len()..])
        {
            return Some(now + wait);
        }
    }
    for marker in ["resets at ", "reset at ", "resets on "] {
        if let Some(index) = text.find(marker) {
            let token = text[index + marker.len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_end_matches(['.', ',', ';']);
            if let Ok(at) = DateTime::parse_from_rfc3339(token) {
                return Some(at.with_timezone(&Utc));
            }
        }
    }
    None
}

fn parse_duration(text: &str) -> Option<chrono::Duration> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && !bytes[index].is_ascii_digit() {
        index += 1;
    }
    let start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if start == index {
        return None;
    }
    let amount: i64 = text[start..index].parse().ok()?;
    let rest = text[index..].trim_start().to_ascii_lowercase();
    let per_unit = if rest.starts_with("hour") || rest.starts_with("hr") {
        3_600
    } else if rest.starts_with("min") {
        60
    } else if rest.starts_with("sec") {
        1
    } else {
        return None;
    };
    let seconds = amount.clamp(0, 86_400).saturating_mul(per_unit);
    Some(chrono::Duration::seconds(seconds.min(86_400)))
}

fn plain_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(300)
        .collect::<String>()
        .trim()
        .to_string()
}

fn decode(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

pub struct CliProvider {
    pub config: ProviderConfig,
    pub vendor: Vendor,
    pub binary: PathBuf,
    /// Result of the version gate, computed on first use.
    version: OnceLock<Result<String, String>>,
}

impl CliProvider {
    /// Resolves the binary; never runs it. Unknown adapter or missing binary → `Unavailable`.
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let Some(vendor) = Vendor::from_adapter(&config.adapter) else {
            return Err(ProviderError::Unavailable(format!(
                "{} is not a CLI adapter",
                config.adapter
            )));
        };
        let binary = resolve_binary(config.command.as_deref(), vendor.default_binary())?;
        Ok(Self {
            config,
            vendor,
            binary,
            version: OnceLock::new(),
        })
    }

    /// The installed version, after the gate passed.
    pub fn version(&self) -> Option<&str> {
        self.version
            .get()
            .and_then(|version| version.as_deref().ok())
    }

    /// Run `--version` once, apply the gate, and cache a definitive outcome. A transient
    /// failure (cancel, timeout, spawn error) is not cached, so the next call retries.
    async fn verify(&self, cancel: &CancellationToken) -> Result<String, ProviderError> {
        if let Some(cached) = self.version.get() {
            return cached.clone().map_err(ProviderError::Unavailable);
        }
        let output = match self
            .run(&["--version".to_string()], "", VERSION_TIMEOUT, cancel)
            .await
        {
            Ok(output) => output,
            Err(ProviderError::Cancelled) => return Err(ProviderError::Cancelled),
            Err(error) => {
                return Err(ProviderError::Unavailable(format!(
                    "{} CLI --version failed: {error:?}",
                    self.vendor.label()
                )));
            }
        };
        let computed = if !output.status.success() {
            Err(format!(
                "{} CLI --version failed: {}",
                self.vendor.label(),
                plain_text(&output.stderr)
            ))
        } else {
            let combined = format!("{}\n{}", output.stdout, output.stderr);
            match parse_semver(&combined) {
                None => Err(format!(
                    "{} CLI did not report a MAJOR.MINOR.PATCH version",
                    self.vendor.label()
                )),
                Some(version) => {
                    let verified = self.vendor.verified_versions();
                    if !self.config.cli_unverified_ok
                        && !verified.iter().any(|prefix| version.starts_with(prefix))
                    {
                        let known = if verified.is_empty() {
                            "no version yet".to_string()
                        } else {
                            verified.join(", ")
                        };
                        Err(format!(
                            "unverified {} CLI version {version}; flags verified for {known}",
                            self.vendor.label()
                        ))
                    } else {
                        Ok(version)
                    }
                }
            }
        };
        let _ = self.version.set(computed.clone());
        computed.map_err(ProviderError::Unavailable)
    }

    async fn run(
        &self,
        args: &[String],
        input: &str,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<RunOutput, ProviderError> {
        let dir = tempfile::TempDir::new().map_err(|error| {
            ProviderError::Unavailable(format!("temporary working directory: {error}"))
        })?;
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(args)
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        for key in KEPT_ENV {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("CI", "1");
        #[cfg(unix)]
        command.process_group(0);
        // A vendor updater can briefly hold the binary open for writing (ETXTBSY);
        // retrying once is enough and matches how package managers replace binaries.
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if is_text_busy(&error) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                command.spawn().map_err(|error| {
                    ProviderError::Unavailable(format!(
                        "{} could not start: {error}",
                        self.binary.display()
                    ))
                })?
            }
            Err(error) => {
                return Err(ProviderError::Unavailable(format!(
                    "{} could not start: {error}",
                    self.binary.display()
                )));
            }
        };
        let group = Group::new(child.id());
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::Http("CLI stdout pipe is missing".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ProviderError::Http("CLI stderr pipe is missing".to_string()))?;
        let run = async {
            if let Some(mut stdin) = stdin {
                // The CLI may exit before reading; its exit status is the authority.
                let _ = stdin.write_all(input.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
            let (stdout, stderr) =
                tokio::join!(read_capped(stdout, &group), read_capped(stderr, &group));
            let stdout = stdout?;
            let stderr = stderr?;
            let status = child.wait().await.map_err(|error| {
                ProviderError::Http(format!(
                    "{} CLI did not finish: {error}",
                    self.vendor.label()
                ))
            })?;
            Ok::<RunOutput, ProviderError>(RunOutput {
                stdout: decode(stdout),
                stderr: decode(stderr),
                status,
            })
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = tokio::time::timeout(timeout, run) => match result {
                Ok(result) => result,
                Err(_) => Err(ProviderError::Http(format!(
                    "{} CLI timed out",
                    self.vendor.label()
                ))),
            },
        };
        if result.is_err() {
            group.kill();
            let _ = child.wait().await;
        }
        result
    }
}

fn is_text_busy(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

struct RunOutput {
    stdout: String,
    stderr: String,
    status: ExitStatus,
}

/// SIGKILL the child's process group after a timeout or cancellation.
struct Group {
    pid: Option<u32>,
}

impl Group {
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // The child is its own group leader (`process_group(0)`).
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

async fn read_capped<R: AsyncRead + Unpin>(
    mut reader: R,
    group: &Group,
) -> Result<Vec<u8>, ProviderError> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).await.map_err(|error| {
            ProviderError::Http(format!("CLI output could not be read: {error}"))
        })?;
        if read == 0 {
            return Ok(output);
        }
        if output.len() + read > MAX_OUTPUT_BYTES {
            group.kill();
            return Err(ProviderError::Http("output too large".to_string()));
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

fn resolve_binary(command: Option<&str>, default_binary: &str) -> Result<PathBuf, ProviderError> {
    let name = command
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .unwrap_or(default_binary);
    if name.contains(std::path::MAIN_SEPARATOR) || name.contains('/') {
        let path = PathBuf::from(name);
        if is_executable(&path) {
            return Ok(path);
        }
        return Err(ProviderError::Unavailable(format!(
            "{} is not installed or not executable",
            path.display()
        )));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(ProviderError::Unavailable(format!(
        "{name} is not installed"
    )))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The first `MAJOR.MINOR.PATCH` in the output.
fn parse_semver(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let start = index;
            let mut major_end = index;
            while major_end < bytes.len() && bytes[major_end].is_ascii_digit() {
                major_end += 1;
            }
            if major_end < bytes.len() && bytes[major_end] == b'.' {
                let major = &text[start..major_end];
                let minor_start = major_end + 1;
                let mut minor_end = minor_start;
                while minor_end < bytes.len() && bytes[minor_end].is_ascii_digit() {
                    minor_end += 1;
                }
                if minor_end > minor_start && minor_end < bytes.len() && bytes[minor_end] == b'.' {
                    let minor = &text[minor_start..minor_end];
                    let patch_start = minor_end + 1;
                    let mut patch_end = patch_start;
                    while patch_end < bytes.len() && bytes[patch_end].is_ascii_digit() {
                        patch_end += 1;
                    }
                    if patch_end > patch_start {
                        let patch = &text[patch_start..patch_end];
                        return Some(format!("{major}.{minor}.{patch}"));
                    }
                }
            }
            index = start + 1;
        } else {
            index += 1;
        }
    }
    None
}

#[async_trait]
impl ExternalProvider for CliProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    async fn complete(
        &self,
        system: &str,
        prompt: &str,
        max_tokens: u32,
        cancel: CancellationToken,
    ) -> Result<ProviderReply, ProviderError> {
        let _ = max_tokens;
        self.verify(&cancel).await?;
        let args = self.vendor.args(&self.config.model);
        let input = format!("{system}\n\n---\n\n{prompt}");
        let output = self.run(&args, &input, RUN_TIMEOUT, &cancel).await?;
        if !output.status.success() {
            return Err(self.vendor.classify_failure(
                output.status.code(),
                &output.stdout,
                &output.stderr,
                Utc::now(),
            ));
        }
        let answer = self.vendor.parse_output(&output.stdout)?;
        if answer.text.trim().is_empty() {
            return Err(ProviderError::Http("empty answer".to_string()));
        }
        let model = answer
            .model
            .filter(|model| !model.is_empty())
            .or_else(|| (!self.config.model.trim().is_empty()).then(|| self.config.model.clone()))
            .unwrap_or_else(|| format!("{} default", self.vendor.label()));
        let input_tokens = answer
            .input_tokens
            .unwrap_or_else(|| chars_to_tokens(&input));
        let output_tokens = answer
            .output_tokens
            .unwrap_or_else(|| chars_to_tokens(&answer.text));
        Ok(ProviderReply {
            text: answer.text,
            model,
            input_tokens,
            output_tokens,
        })
    }
}

fn chars_to_tokens(text: &str) -> u64 {
    (text.chars().count() / 4) as u64
}
