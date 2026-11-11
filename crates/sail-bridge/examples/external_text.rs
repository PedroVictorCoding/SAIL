//! Owner-terminal demo of the guarded API path. No provider is enabled in the daemon.
//! Credentials must already exist in Guardian's vault as `external-<provider>` with
//! scope `external:<provider>:infer`. Never put a key on the command line.
use sail_common::resources;
use sail_guardian::{GuardianSubsystem, egress::prepare_text};
use sail_mind::external::{ExternalBackend, ExternalProviderConfig, ledger::UsageLedger};
use sail_protocol::{ErrorCode, InferenceRequest, ResourceLimits};
use std::{
    io::{self, IsTerminal, Write},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(
            "usage (interactive owner terminal): external_text HTTPS_ENDPOINT PROVIDER MODEL"
                .into(),
        );
    }
    let ledger = Arc::new(UsageLedger::open_default()?);
    let root = sail_common::storage::data_root();
    let guardian = GuardianSubsystem::with_data_dir(&root.join("guardian"));
    let vault = guardian.credentials();
    let handle = vault.get_handle(
        sail_common::types::SubsystemId::Bridge,
        &format!("external-{}", args[1]),
        &format!("external:{}:infer", args[1]),
    )?;
    let transport = Arc::new(sail_bridge::external::OpenAiTextConnector::new(
        &args[0],
        &args[1],
        vault.clone(),
        handle,
    )?);
    let backend = ExternalBackend::new(
        ExternalProviderConfig {
            provider: args[1].clone(),
            model: args[2].clone(),
            daily_cap: 10,
            entity_names: vec![],
        },
        transport,
        ledger.clone(),
        guardian.broker(),
        vault.clone(),
        resources::global(),
    )?;

    eprint!("Prompt (one line; no retrieved memory): ");
    io::stderr().flush()?;
    let mut prompt = String::new();
    io::stdin().read_line(&mut prompt)?;
    let request = InferenceRequest::text(prompt.trim());
    let limits = ResourceLimits::default();
    let prepared = prepare_text(&request, &vault, &[])?;
    eprintln!(
        "\nSanitized prompt:\n{}\n\nProvider: {}\nRequest digest: {}",
        prepared.text(),
        args[1],
        prepared.sha256()
    );
    eprintln!(
        "This demo has no Arena entity inventory. Check the preview for names and private information."
    );
    let capability = backend.approval_capability(&request, &limits)?;
    let session = guardian.approval().create_session_for(
        Some(request.request_id),
        sail_common::types::SubsystemId::Mind,
        capability,
        "explicit owner-terminal external request",
    );
    eprint!("Type send to authorize this one request, or anything else to cancel: ");
    io::stderr().flush()?;
    let mut consent = String::new();
    io::stdin().read_line(&mut consent)?;
    if consent.trim() != "send" {
        let _ = guardian
            .approval()
            .resolve_session(session.session_id, false);
        return Ok(());
    }
    let token = guardian
        .approval()
        .resolve_session(session.session_id, true)?;
    match backend
        .infer_authorized(
            &request,
            Some(token.token_id),
            &CancellationToken::new(),
            &limits,
        )
        .await
    {
        Ok(response) => {
            println!(
                "{}\n{}",
                response.model,
                response.message.content.text().unwrap_or_default()
            );
            eprintln!(
                "Egress receipt: {}",
                serde_json::to_string(&ledger.receipt(request.request_id)?)?
            );
        }
        Err(error) if error.code == ErrorCode::Busy => return Err(
            "background inference is paused; retry after live voice or resource pressure clears"
                .into(),
        ),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
