//! Fake vendor CLIs in a temp dir: no network, no real `claude`, `codex` or `gemini`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use sail_mind::escalation::ProviderConfig;
use sail_mind::escalation::cli::{CliProvider, Vendor};
use sail_mind::escalation::provider::{ExternalProvider, ProviderError, ProviderReply};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const VENDORS: [&str; 3] = ["claude_cli", "codex_cli", "gemini_cli"];

struct Fake {
    _dir: TempDir,
    bin: PathBuf,
    dump: PathBuf,
}

fn write_cli(dir: &Path, version_output: &str, body: &str) -> (PathBuf, PathBuf) {
    let bin = dir.join("fake-cli");
    let dump = dir.join("dump.txt");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then\n\
           printf '%s\\n' '{version_output}'\n\
           exit 0\n\
         fi\n\
         {body}\n"
    );
    std::fs::write(&bin, script).expect("write fake CLI");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .expect("make fake CLI executable");
    }
    (bin, dump)
}

fn program(version_output: &str, body: &str) -> Fake {
    let dir = tempfile::tempdir().expect("temp dir");
    let (bin, dump) = write_cli(dir.path(), version_output, body);
    Fake {
        _dir: dir,
        bin,
        dump,
    }
}

fn config_for(adapter: &str, command: &Path, model: &str, unverified_ok: bool) -> ProviderConfig {
    ProviderConfig {
        id: format!("{adapter}-test"),
        adapter: adapter.to_string(),
        base_url: String::new(),
        model: model.to_string(),
        api_key_env: String::new(),
        command: Some(command.display().to_string()),
        cli_unverified_ok: unverified_ok,
        kinds: Vec::new(),
        input_price_micros_per_mtok: 0,
        output_price_micros_per_mtok: 0,
        enabled: true,
    }
}

fn config(adapter: &str, fake: &Fake, model: &str, unverified_ok: bool) -> ProviderConfig {
    config_for(adapter, &fake.bin, model, unverified_ok)
}

fn fake_provider(adapter: &str, fake: &Fake, model: &str) -> CliProvider {
    CliProvider::new(config(adapter, fake, model, true)).expect("fake CLI provider")
}

async fn run(provider: &CliProvider) -> Result<ProviderReply, ProviderError> {
    provider
        .complete(
            "system instructions",
            "user question",
            1_000,
            CancellationToken::new(),
        )
        .await
}

#[test]
fn every_vendor_has_the_pinned_flags_with_and_without_a_model() {
    assert_eq!(
        Vendor::ClaudeCode.args("sonnet"),
        [
            "-p",
            "--output-format",
            "json",
            "--tools",
            "",
            "--strict-mcp-config",
            "--permission-mode",
            "default",
            "--no-session-persistence",
            "--model",
            "sonnet",
        ]
    );
    assert_eq!(
        Vendor::Codex.args("gpt-5-codex"),
        [
            "exec",
            "--json",
            "--sandbox",
            "read-only",
            "--skip-git-repo-check",
            "--ephemeral",
            "--color",
            "never",
            "-m",
            "gpt-5-codex",
            "-",
        ]
    );
    assert_eq!(
        Vendor::Gemini.args("gemini-3-pro"),
        [
            "--output-format",
            "json",
            "--approval-mode",
            "default",
            "--sandbox",
            "-m",
            "gemini-3-pro",
        ]
    );
    for vendor in [Vendor::ClaudeCode, Vendor::Codex, Vendor::Gemini] {
        let args = vendor.args("");
        assert!(!args.iter().any(|arg| arg == "--model" || arg == "-m"));
        if vendor == Vendor::Codex {
            assert_eq!(args.last().map(String::as_str), Some("-"));
        }
    }
}

#[tokio::test]
async fn claude_success_parses_result_model_and_usage() {
    let fake = program(
        "2.1.118 (Claude Code)",
        r#"printf '%s' '{"result":"Use a queue.","is_error":false,"usage":{"input_tokens":11,"output_tokens":7},"modelUsage":{"claude-sonnet-5":{"outputTokens":7}}}'"#,
    );
    let provider = fake_provider("claude_cli", &fake, "configured-model");
    let reply = run(&provider).await.expect("reply");
    assert_eq!(reply.text, "Use a queue.");
    assert_eq!(reply.model, "claude-sonnet-5");
    assert_eq!(reply.input_tokens, 11);
    assert_eq!(reply.output_tokens, 7);
    assert_eq!(provider.version(), Some("2.1.118"));
}

#[tokio::test]
async fn codex_success_parses_the_last_agent_message_and_turn_usage() {
    let fake = program(
        "codex-cli 0.120.0",
        r#"printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"first draft"}}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"Nowhere to hide."}}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":40,"output_tokens":20}}'"#,
    );
    let provider = fake_provider("codex_cli", &fake, "gpt-5-codex");
    let reply = run(&provider).await.expect("reply");
    assert_eq!(reply.text, "Nowhere to hide.");
    assert_eq!(reply.model, "gpt-5-codex");
    assert_eq!(reply.input_tokens, 40);
    assert_eq!(reply.output_tokens, 20);
}

#[tokio::test]
async fn gemini_success_parses_response_model_and_tokens() {
    let fake = program(
        "0.30.1",
        r#"printf '%s' '{"response":"Trust the tests.","stats":{"models":{"gemini-3-pro-preview":{"tokens":{"prompt":7,"candidates":3}}}}}'"#,
    );
    let provider = fake_provider("gemini_cli", &fake, "configured-model");
    let reply = run(&provider).await.expect("reply");
    assert_eq!(reply.text, "Trust the tests.");
    assert_eq!(reply.model, "gemini-3-pro-preview");
    assert_eq!(reply.input_tokens, 7);
    assert_eq!(reply.output_tokens, 3);
}

#[tokio::test]
async fn token_counts_fall_back_to_chars_over_four() {
    let fake = program(
        "codex-cli 0.120.0",
        r#"printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"12345678"}}'"#,
    );
    let provider = fake_provider("codex_cli", &fake, "");
    let reply = run(&provider).await.expect("reply");
    assert_eq!(reply.output_tokens, 2);
    assert!(reply.input_tokens > 0);
    assert_eq!(reply.model, "Codex default");
}

#[tokio::test]
async fn every_vendor_reports_a_rate_limit_with_and_without_a_reset_time() {
    for adapter in VENDORS {
        let fake = program(
            "1.2.3",
            "echo '429 rate limit exceeded; try again in 5 minutes' >&2\nexit 1",
        );
        let provider = fake_provider(adapter, &fake, "model");
        match run(&provider).await.expect_err("rate limited") {
            ProviderError::RateLimited { retry_after } => {
                let wait = (retry_after.expect("reset time") - Utc::now()).num_seconds();
                assert!(
                    (240..=360).contains(&wait),
                    "{adapter}: unexpected wait {wait}s"
                );
            }
            other => panic!("{adapter}: unexpected error {other:?}"),
        }

        let fake = program("1.2.3", "echo 'quota exceeded' >&2\nexit 1");
        let provider = fake_provider(adapter, &fake, "model");
        match run(&provider).await.expect_err("rate limited") {
            ProviderError::RateLimited { retry_after } => assert!(retry_after.is_none()),
            other => panic!("{adapter}: unexpected error {other:?}"),
        }
    }
}

#[tokio::test]
async fn every_vendor_maps_auth_failures_to_unavailable() {
    for adapter in VENDORS {
        let fake = program(
            "1.2.3",
            "echo 'Not signed in. Run the vendor login command.' >&2\nexit 1",
        );
        let provider = fake_provider(adapter, &fake, "model");
        match run(&provider).await.expect_err("not signed in") {
            ProviderError::Unavailable(message) => {
                assert!(message.contains("not signed in"), "{adapter}: {message}");
            }
            other => panic!("{adapter}: unexpected error {other:?}"),
        }
    }
}

#[tokio::test]
async fn every_vendor_maps_other_failures_to_http_with_clean_text() {
    for adapter in VENDORS {
        let fake = program("1.2.3", "printf 'boom\\001\\n' >&2\nexit 3");
        let provider = fake_provider(adapter, &fake, "model");
        match run(&provider).await.expect_err("failed") {
            ProviderError::Http(message) => {
                assert!(message.contains("exited with 3"), "{adapter}: {message}");
                assert!(message.contains("boom"), "{adapter}: {message}");
                assert!(!message.contains('\u{1}'), "{adapter}: {message}");
            }
            other => panic!("{adapter}: unexpected error {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_empty_answer_is_an_error_not_a_reply() {
    let fake = program(
        "2.1.118",
        r#"printf '%s' '{"result":"  ","is_error":false}'"#,
    );
    let provider = fake_provider("claude_cli", &fake, "model");
    assert_eq!(
        run(&provider).await.expect_err("empty"),
        ProviderError::Http("empty answer".to_string())
    );

    let fake = program(
        "0.120.0",
        r#"printf '%s' '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'"#,
    );
    let provider = fake_provider("codex_cli", &fake, "model");
    assert!(matches!(
        run(&provider).await,
        Err(ProviderError::Http(message)) if message.contains("no agent message")
    ));
}

#[tokio::test]
async fn a_cli_reported_error_never_becomes_an_answer() {
    let fake = program(
        "2.1.118",
        r#"printf '%s' '{"result":"Credit balance is too low","is_error":true}'"#,
    );
    let provider = fake_provider("claude_cli", &fake, "model");
    assert!(matches!(
        run(&provider).await,
        Err(ProviderError::Http(message)) if message.contains("Credit balance is too low")
    ));

    let fake = program(
        "0.30.1",
        r#"printf '%s' '{"response":"","error":{"message":"invalid request"}}'"#,
    );
    let provider = fake_provider("gemini_cli", &fake, "model");
    assert!(matches!(
        run(&provider).await,
        Err(ProviderError::Http(message)) if message.contains("invalid request")
    ));
}

#[tokio::test]
async fn the_prompt_arrives_on_stdin_and_the_sandbox_strips_sail_variables() {
    unsafe { std::env::set_var("SAIL_TEST_CLI_SECRET_449", "must-not-leak") };
    let dir = tempfile::tempdir().expect("temp dir");
    let dump = dir.path().join("dump.txt");
    let body = format!(
        r#"{{
  echo "PWD=$PWD"
  echo "EMPTY=$(ls -A | wc -l | tr -d ' ')"
  echo "SAIL=$(env | grep -c '^SAIL_' || true)"
  echo "TERM=$TERM"
  echo "CI=$CI"
  echo "NO_COLOR=$NO_COLOR"
  for arg in "$@"; do printf 'ARG=%s\n' "$arg"; done
  echo "STDIN_BEGIN"
  cat
  printf '\nSTDIN_END\n'
}} > '{}'
printf '%s' '{{"result":"seen","is_error":false}}'"#,
        dump.display()
    );
    let (bin, dump) = write_cli(dir.path(), "2.1.118", &body);
    let fake = Fake {
        _dir: dir,
        bin,
        dump,
    };
    let provider = fake_provider("claude_cli", &fake, "model");
    let reply = run(&provider).await.expect("reply");
    assert_eq!(reply.text, "seen");

    let dump = std::fs::read_to_string(&fake.dump).expect("dump");
    assert!(dump.contains("SAIL=0"), "{dump}");
    assert!(!dump.contains("must-not-leak"), "{dump}");
    assert!(dump.contains("EMPTY=0"), "{dump}");
    assert!(dump.contains("TERM=dumb"), "{dump}");
    assert!(dump.contains("CI=1"), "{dump}");
    assert!(dump.contains("NO_COLOR=1"), "{dump}");
    assert!(
        !dump.contains(env!("CARGO_MANIFEST_DIR")),
        "the CLI ran in the source tree: {dump}"
    );
    assert!(dump.contains("ARG=-p"), "{dump}");
    assert!(dump.contains("ARG=--output-format"), "{dump}");
    assert!(!dump.contains("ARG=user question"), "{dump}");
    assert!(
        dump.contains("system instructions\n\n---\n\nuser question"),
        "{dump}"
    );
}

#[tokio::test]
async fn a_missing_binary_is_unavailable_from_new() {
    match CliProvider::new(config_for(
        "claude_cli",
        Path::new("/definitely/not/installed/claude"),
        "model",
        true,
    )) {
        Err(ProviderError::Unavailable(message)) => {
            assert!(message.contains("not installed"), "{message}")
        }
        Ok(_) => panic!("a missing binary must not produce a provider"),
        Err(other) => panic!("unexpected error {other:?}"),
    }

    let fake = program("1.2.3", "exit 0");
    let plain = fake.bin.parent().unwrap().join("plain.txt");
    std::fs::write(&plain, "not a program").expect("plain file");
    assert!(matches!(
        CliProvider::new(config_for("claude_cli", &plain, "model", true)),
        Err(ProviderError::Unavailable(_))
    ));

    let mut config = config("claude_cli", &fake, "model", true);
    config.adapter = "not_a_vendor".to_string();
    assert!(matches!(
        CliProvider::new(config),
        Err(ProviderError::Unavailable(_))
    ));
}

#[tokio::test]
async fn new_does_not_run_the_cli() {
    let dir = tempfile::tempdir().expect("temp dir");
    let dump = dir.path().join("ran.txt");
    let body = format!("echo ran >> '{}'", dump.display());
    let (bin, _) = write_cli(dir.path(), "1.2.3", &body);
    let provider =
        CliProvider::new(config_for("claude_cli", &bin, "model", true)).expect("provider");
    assert!(!dump.exists(), "CliProvider::new must not execute the CLI");
    assert_eq!(provider.version(), None);
}

#[tokio::test]
async fn an_unverified_version_needs_the_policy_opt_in() {
    let fake = program("9.9.9", r#"printf '%s' '{"result":"ok"}'"#);
    let provider = CliProvider::new(config("claude_cli", &fake, "model", false)).expect("provider");
    match run(&provider).await {
        Err(ProviderError::Unavailable(message)) => {
            assert!(message.contains("unverified"), "{message}");
            assert!(message.contains("9.9.9"), "{message}");
            assert!(message.contains("Claude Code"), "{message}");
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(provider.version(), None);

    let provider = CliProvider::new(config("claude_cli", &fake, "model", true)).expect("provider");
    assert_eq!(run(&provider).await.expect("opt-in").text, "ok");
    assert_eq!(provider.version(), Some("9.9.9"));
}

#[tokio::test]
async fn a_failing_version_command_is_unavailable() {
    let dir = tempfile::tempdir().expect("temp dir");
    let bin = dir.path().join("fake-cli");
    std::fs::write(
        &bin,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'no version for you' >&2; exit 4; fi\n",
    )
    .expect("script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let fake = Fake {
        _dir: dir,
        bin,
        dump: PathBuf::new(),
    };
    let provider = fake_provider("claude_cli", &fake, "model");
    match run(&provider).await {
        Err(ProviderError::Unavailable(message)) => {
            assert!(message.contains("--version failed"), "{message}")
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn output_over_the_cap_fails_instead_of_truncating() {
    let fake = program(
        "1.2.3",
        "dd if=/dev/zero bs=1024 count=2048 2>/dev/null | tr '\\0' 'x'",
    );
    let provider = fake_provider("claude_cli", &fake, "model");
    match run(&provider).await {
        Err(ProviderError::Http(message)) => assert_eq!(message, "output too large"),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn cancelling_kills_a_sleeping_script_quickly() {
    let fake = program("1.2.3", "sleep 30\nprintf '%s' '{\"result\":\"too late\"}'");
    let provider = fake_provider("codex_cli", &fake, "model");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let error = provider
        .complete("system", "question", 1_000, cancel)
        .await
        .expect_err("must be cancelled");
    assert_eq!(error, ProviderError::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cancel took {:?}",
        started.elapsed()
    );
}

#[test]
fn build_returns_a_cli_provider_for_each_adapter_name() {
    let fake = program("1.2.3", "exit 0");
    for adapter in VENDORS {
        let config = config(adapter, &fake, "model", true);
        let built = sail_mind::escalation::provider::build(&config).expect("CLI provider built");
        assert_eq!(built.id(), format!("{adapter}-test"));
        assert_eq!(built.model(), "model");
    }
}
