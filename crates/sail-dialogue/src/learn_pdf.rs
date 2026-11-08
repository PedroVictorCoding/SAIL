//! Course Factory adapter for the fixed runner PDF protocol. Source bytes stay local;
//! modern PDFs are never parsed by a subprocess running on the daemon's UID.
use sail_arena_learn::factory::contracts::{
    PDF_TEXT_PAGES_PER_CALL, PdfPageText, PdfTextError, PdfTextExtractor,
};
use sail_guardian::GuardianExecutionBroker;
use sail_protocol::execution::{CommandResult, CommandStatus};
use sail_runner::pdf::{PdfOperation, PdfRequest};
use std::{io::Read, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

/// The shared broker enforces execution authority; unconfigured runners stay unavailable.
#[derive(Clone)]
pub struct RunnerPdfText {
    socket: PathBuf,
    runner_uid: u32,
    blob_dir: PathBuf,
    broker: Arc<GuardianExecutionBroker>,
}
impl RunnerPdfText {
    /// Mirror the Rust evaluator's optional runner configuration. `blob_dir` is the same
    /// mounted local directory handed to SourceRegistry, never a path from a document.
    pub fn from_env(blob_dir: PathBuf, broker: Arc<GuardianExecutionBroker>) -> Option<Self> {
        Some(Self {
            socket: std::env::var_os("SAIL_RUNNER_SOCKET")?.into(),
            runner_uid: std::env::var("SAIL_RUNNER_UID").ok()?.parse().ok()?,
            blob_dir,
            broker,
        })
    }
    /// Prepare a request for an owner approval flow. Keep this exact request when retrying
    /// with a returned token; request ids and operation are part of the granted capability.
    pub fn prepare(
        &self,
        content_hash: &str,
        operation: PdfOperation,
    ) -> Result<PdfRequest, PdfTextError> {
        let bytes = self.read_blob(content_hash)?;
        let request =
            PdfRequest::new(&bytes, uuid::Uuid::new_v4(), operation).map_err(input_error)?;
        request.validate().map_err(input_error)?;
        Ok(request)
    }
    /// Run an already prepared request with its optional exact-invocation grant. Call this
    /// on a blocking worker. A denied grant never causes an in-process fallback.
    pub fn run_prepared(
        &self,
        request: &PdfRequest,
        token: Option<uuid::Uuid>,
        cancel: CancellationToken,
    ) -> Result<CommandResult, PdfTextError> {
        if cancel.is_cancelled() {
            return Err(PdfTextError::Failed("extraction cancelled".into()));
        }
        let bytes = self.read_blob(&request.input_sha256)?;
        let result = sail_runner::pdf::dispatch(
            &self.socket,
            self.runner_uid,
            &self.broker,
            request,
            &bytes,
            token,
            cancel,
        )
        .map_err(|error| match error {
            sail_guardian::ExecutionDenied::ApprovalRequired { session_id, .. } => {
                PdfTextError::Unavailable(format!(
                    "Guardian approval required; session {session_id}; request {}",
                    request.request_id
                ))
            }
            sail_guardian::ExecutionDenied::GuardianUnavailable => {
                PdfTextError::Unavailable("Guardian unavailable".into())
            }
            _ => PdfTextError::Refused("Guardian refused PDF extraction".into()),
        })?
        .map_err(|_| PdfTextError::Unavailable("PDF runner cannot be reached".into()))?;
        checked_result(result)
    }
    fn read_blob(&self, hash: &str) -> Result<Vec<u8>, PdfTextError> {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(PdfTextError::Refused("invalid source content hash".into()));
        }
        let path = self.blob_dir.join(hash);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| PdfTextError::Failed("source blob unavailable".into()))?;
        if !metadata.is_file() || metadata.len() > sail_runner::pdf::MAX_INPUT_BYTES as u64 {
            return Err(PdfTextError::Refused(
                "invalid or oversized source blob".into(),
            ));
        }
        let file = std::fs::File::open(path)
            .map_err(|_| PdfTextError::Failed("source blob unavailable".into()))?;
        let mut bytes = Vec::new();
        file.take(sail_runner::pdf::MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PdfTextError::Failed("source blob read failed".into()))?;
        if sail_arena_learn::factory::contracts::sha256_hex(&bytes) != hash {
            return Err(PdfTextError::Refused("source blob digest mismatch".into()));
        }
        sail_runner::pdf::validate_input(&bytes).map_err(input_error)?;
        Ok(bytes)
    }
    async fn run(
        &self,
        hash: &str,
        operation: PdfOperation,
        cancel: CancellationToken,
    ) -> Result<CommandResult, PdfTextError> {
        let adapter = self.clone();
        let hash = hash.to_string();
        tokio::task::spawn_blocking(move || {
            if cancel.is_cancelled() {
                return Err(PdfTextError::Failed("extraction cancelled".into()));
            }
            let request = adapter.prepare(&hash, operation)?;
            adapter.run_prepared(&request, None, cancel)
        })
        .await
        .map_err(|_| PdfTextError::Failed("PDF worker stopped".into()))?
    }
}
fn input_error(error: std::io::Error) -> PdfTextError {
    PdfTextError::Refused(error.to_string())
}
fn checked_result(result: CommandResult) -> Result<CommandResult, PdfTextError> {
    if result.status == CommandStatus::Unavailable {
        return Err(PdfTextError::Unavailable("PDF runner unavailable".into()));
    }
    if result.status == CommandStatus::Denied {
        return Err(PdfTextError::Refused(
            "PDF runner refused the source".into(),
        ));
    }
    if result.status != CommandStatus::Completed
        || result.exit_code != Some(0)
        || result.output_truncated
    {
        return Err(PdfTextError::Failed(format!(
            "PDF extraction did not complete ({:?})",
            result.status
        )));
    }
    if !result.stderr.is_empty() {
        return Err(PdfTextError::Failed(
            "PDF extractor emitted diagnostics; no partial output accepted".into(),
        ));
    }
    Ok(result)
}
fn parse_page_count(text: &str) -> Result<u32, PdfTextError> {
    if text.lines().any(|line| {
        line.strip_prefix("Encrypted:")
            .is_some_and(|value| value.trim().starts_with("yes"))
    }) {
        return Err(PdfTextError::Refused(
            "encrypted PDF; no decryption permitted".into(),
        ));
    }
    let encrypted: Vec<_> = text
        .lines()
        .filter_map(|l| l.strip_prefix("Encrypted:"))
        .collect();
    let counts: Vec<_> = text
        .lines()
        .filter_map(|l| l.strip_prefix("Pages:"))
        .collect();
    if encrypted.len() != 1 || encrypted[0].trim() != "no" || counts.len() != 1 {
        return Err(PdfTextError::Failed("ambiguous PDF metadata".into()));
    }
    let count: u32 = counts[0]
        .trim()
        .parse()
        .map_err(|_| PdfTextError::Failed("invalid page count".into()))?;
    if count == 0 || count as usize > sail_arena_learn::factory::ingest::MAX_PAGES {
        return Err(PdfTextError::Refused(
            "PDF page count exceeds source limits".into(),
        ));
    }
    Ok(count)
}
fn split_pages(text: &str, first: u32, last: u32) -> Result<Vec<PdfPageText>, PdfTextError> {
    if first == 0
        || last < first
        || last as usize > sail_arena_learn::factory::ingest::MAX_PAGES
        || last - first + 1 > PDF_TEXT_PAGES_PER_CALL
    {
        return Err(PdfTextError::Refused("invalid page range".into()));
    }
    let body = text
        .strip_suffix('\u{c}')
        .ok_or_else(|| PdfTextError::Failed("missing PDF page terminator".into()))?;
    let parts: Vec<_> = body.split('\u{c}').collect();
    if parts.len() != (last - first + 1) as usize {
        return Err(PdfTextError::Failed(
            "PDF output page count mismatch".into(),
        ));
    }
    Ok(parts
        .into_iter()
        .enumerate()
        .map(|(i, text)| PdfPageText {
            page: first + i as u32,
            label: None,
            text: text.into(),
        })
        .collect())
}
#[async_trait::async_trait]
impl PdfTextExtractor for RunnerPdfText {
    async fn page_count(&self, content_hash: &str) -> Result<u32, PdfTextError> {
        let result = self
            .run(
                content_hash,
                PdfOperation::PageCount,
                CancellationToken::new(),
            )
            .await?;
        parse_page_count(&result.stdout)
    }
    async fn pages(
        &self,
        content_hash: &str,
        first: u32,
        last: u32,
        cancel: CancellationToken,
    ) -> Result<Vec<PdfPageText>, PdfTextError> {
        if first == 0
            || last < first
            || last as usize > sail_arena_learn::factory::ingest::MAX_PAGES
            || last - first + 1 > PDF_TEXT_PAGES_PER_CALL
        {
            return Err(PdfTextError::Refused("invalid page range".into()));
        }
        let result = self
            .run(content_hash, PdfOperation::Pages { first, last }, cancel)
            .await?;
        split_pages(&result.stdout, first, last)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_boundaries_and_metadata_are_strict() {
        assert_eq!(sail_runner::pdf::PAGES_PER_CALL, PDF_TEXT_PAGES_PER_CALL);
        assert_eq!(parse_page_count("Pages: 42\nEncrypted: no\n").unwrap(), 42);
        assert!(matches!(
            parse_page_count("Pages: 42\nEncrypted: yes (print:yes)"),
            Err(PdfTextError::Refused(_))
        ));
        assert!(parse_page_count("Pages: 1\nPages: 500\nEncrypted: no").is_err());
        let pages = split_pages("First\u{c}\u{c}", 11, 12).unwrap();
        assert_eq!(pages[0].page, 11);
        assert_eq!(pages[1].text, "");
        assert!(split_pages("First\u{c}", 11, 12).is_err());
        assert!(split_pages("First", 1, 1).is_err());
    }
    #[test]
    fn failure_and_truncated_results_cannot_supply_fragments() {
        let result = CommandResult {
            version: 1,
            request_id: uuid::Uuid::new_v4(),
            correlation_id: uuid::Uuid::new_v4(),
            status: CommandStatus::Completed,
            exit_code: Some(0),
            stdout: "partial".into(),
            stderr: String::new(),
            output_truncated: true,
            elapsed_ms: 0,
        };
        assert!(checked_result(result.clone()).is_err());
        for status in [
            CommandStatus::TimedOut,
            CommandStatus::Cancelled,
            CommandStatus::OutputLimit,
            CommandStatus::Unavailable,
            CommandStatus::Denied,
        ] {
            let mut changed = result.clone();
            changed.output_truncated = false;
            changed.status = status;
            assert!(checked_result(changed).is_err());
        }
    }
}
