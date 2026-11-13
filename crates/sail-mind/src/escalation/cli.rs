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

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use super::ProviderConfig;
use super::provider::{ExternalProvider, ProviderError, ProviderReply};

/// Per stream (stdout, stderr).
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// One CLI run, end to end.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(180);
/// `--version`.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

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
        let _ = model;
        unimplemented!("Kilo: pinned non-interactive, tool-free argv per vendor")
    }

    /// Parse stdout of a run that exited 0.
    pub fn parse_output(self, stdout: &str) -> Result<CliAnswer, ProviderError> {
        let _ = stdout;
        unimplemented!("Kilo: parse each vendor's JSON / JSONL")
    }

    /// Map a non-zero exit to an error (rate limit, not signed in, other).
    pub fn classify_failure(
        self,
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
        now: DateTime<Utc>,
    ) -> ProviderError {
        let _ = (exit_code, stdout, stderr, now);
        unimplemented!("Kilo: rate-limit / auth / other classification")
    }
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
        let _ = vendor;
        unimplemented!("Kilo: resolve config.command or the default binary on PATH")
    }

    /// The installed version, after the gate passed.
    pub fn version(&self) -> Option<&str> {
        self.version.get().and_then(|v| v.as_deref().ok())
    }
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
        let _ = (system, prompt, max_tokens, cancel);
        unimplemented!("Kilo: version gate, sandboxed run, parse or classify")
    }
}
