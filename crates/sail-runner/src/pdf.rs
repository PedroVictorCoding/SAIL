//! Fixed-purpose PDF text extraction. Binary input is digest-bound to Guardian approval;
//! no host paths, shell commands, passwords, or caller-selected executables cross this port.
use super::*;
use sail_common::types::SubsystemId;
use serde::{Deserialize, Serialize};

/// Distinguish the bounded binary upload from the legacy small command protocol.
pub const PROTOCOL: &str = "pdf-text-v1";
/// Keep two concurrent uploads within a bounded service-memory envelope.
pub const MAX_INPUT_BYTES: usize = 256 * 1024 * 1024;
/// Bound text independently of compressed input; excess output fails the entire import.
pub const MAX_TEXT_BYTES: usize = 128 * 1024;
/// JSON may escape each output byte into six bytes.
pub const MAX_RESULT_FRAME: usize = 2 * 1024 * 1024;

/// Match the shared factory contract without depending on the learning crate.
pub const PAGES_PER_CALL: u32 = 20;

/// Only fixed Poppler operations are available; callers cannot supply argv or paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum PdfOperation {
    /// Inspect the physical page count and encryption flag.
    PageCount,
    /// Extract one bounded, inclusive physical page range.
    Pages {
        /// First 1-based physical page.
        first: u32,
        /// Last 1-based physical page.
        last: u32,
    },
}

/// The exact extraction identity and input digest are approved without recording book text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfRequest {
    /// Prevent a PDF upload from being interpreted as another execution protocol.
    pub protocol: String,
    /// Bind the result and Guardian receipt to this invocation.
    pub request_id: Uuid,
    /// Tie execution to the importing job.
    pub correlation_id: Uuid,
    /// Bound allocation before the binary payload is received.
    pub input_bytes: usize,
    /// Bind the entire document, not merely its filename, to authorization.
    pub input_sha256: String,
    /// Fixed operation included in the capability digest.
    pub operation: PdfOperation,
}
impl PdfRequest {
    /// Hash a bounded document before requesting authority.
    pub fn new(input: &[u8], correlation_id: Uuid, operation: PdfOperation) -> io::Result<Self> {
        validate_input(input)?;
        Ok(Self {
            protocol: PROTOCOL.into(),
            request_id: Uuid::new_v4(),
            correlation_id,
            input_bytes: input.len(),
            input_sha256: digest(input),
            operation,
        })
    }
    /// Reject invalid framing before allocating or launching a parser.
    pub fn validate(&self) -> io::Result<()> {
        if self.protocol != PROTOCOL
            || self.input_bytes < 5
            || self.input_bytes > MAX_INPUT_BYTES
            || self.input_sha256.len() != 64
            || !self
                .input_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(io::Error::other("invalid PDF request or input limit"));
        }
        if let PdfOperation::Pages { first, last } = self.operation
            && (first == 0 || last < first || last > 5000 || last - first + 1 > PAGES_PER_CALL)
        {
            return Err(io::Error::other("PDF page range limit"));
        }
        Ok(())
    }
}
fn digest(input: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, input)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Refuse encryption before invoking Poppler, including escaped PDF names. False positives
/// inside strings/streams are deliberate: extraction never tries empty-password decryption.
pub fn validate_input(input: &[u8]) -> io::Result<()> {
    if input.len() > MAX_INPUT_BYTES || !input.starts_with(b"%PDF-") {
        return Err(io::Error::other("invalid PDF or input byte limit"));
    }
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'/' {
            index += 1;
            continue;
        }
        index += 1;
        let mut name = Vec::new();
        while index < input.len()
            && !input[index].is_ascii_whitespace()
            && !b"/[]<>(){}%".contains(&input[index])
            && name.len() < 128
        {
            if input[index] == b'#'
                && index + 2 < input.len()
                && let (Some(a), Some(b)) = (
                    (input[index + 1] as char).to_digit(16),
                    (input[index + 2] as char).to_digit(16),
                )
            {
                name.push((a * 16 + b) as u8);
                index += 3;
                continue;
            }
            name.push(input[index]);
            index += 1;
        }
        if name == b"Encrypt" {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "encrypted PDF refused; no decryption attempted",
            ));
        }
    }
    Ok(())
}
fn launch(request: &PdfRequest, input: Vec<u8>) -> LaunchSpec {
    let (executable, argv) = match request.operation {
        PdfOperation::PageCount => ("/usr/bin/pdfinfo", vec!["-".into()]),
        PdfOperation::Pages { first, last } => (
            "/usr/bin/pdftotext",
            vec![
                "-layout".into(),
                "-enc".into(),
                "UTF-8".into(),
                "-eol".into(),
                "unix".into(),
                "-f".into(),
                first.to_string(),
                "-l".into(),
                last.to_string(),
                "-".into(),
                "-".into(),
            ],
        ),
    };
    LaunchSpec {
        request_id: request.request_id,
        correlation_id: request.correlation_id,
        executable: executable.into(),
        argv,
        cwd: "/work".into(),
        limits: CommandLimits {
            output_bytes: 2 * MAX_TEXT_BYTES,
            ..CommandLimits::default()
        },
        write_roots: vec!["/work".into(), "/tmp".into()],
        input: Some(input),
    }
}
pub(super) async fn execute(
    config: &RunnerConfig,
    request: &PdfRequest,
    input: Vec<u8>,
    cancel: CancellationToken,
) -> CommandResult {
    let start = Instant::now();
    let valid = request
        .validate()
        .and_then(|()| validate_input(&input))
        .and_then(|()| {
            if input.len() == request.input_bytes && digest(&input) == request.input_sha256 {
                Ok(())
            } else {
                Err(io::Error::other("PDF input digest mismatch"))
            }
        });
    if let Err(error) = valid {
        return empty_result(
            request.request_id,
            request.correlation_id,
            CommandStatus::Denied,
            error.to_string(),
            start,
        );
    }
    if let Err(error) = config.validate() {
        return empty_result(
            request.request_id,
            request.correlation_id,
            CommandStatus::Unavailable,
            error.to_string(),
            start,
        );
    }
    if cancel.is_cancelled() {
        return empty_result(
            request.request_id,
            request.correlation_id,
            CommandStatus::Cancelled,
            "PDF extraction cancelled".into(),
            start,
        );
    }
    let executable = match request.operation {
        PdfOperation::PageCount => "usr/bin/pdfinfo",
        PdfOperation::Pages { .. } => "usr/bin/pdftotext",
    };
    if !config.rootfs.join(executable).is_file() {
        return empty_result(
            request.request_id,
            request.correlation_id,
            CommandStatus::Unavailable,
            "Poppler is missing from the runner image".into(),
            start,
        );
    }
    let mut result = execute_spec(config, launch(request, input), cancel, start).await;
    // Parser diagnostics can contain source text. Keep them out of receipts/logs.
    result.stderr = if result.stderr.is_empty() {
        String::new()
    } else {
        "PDF extractor emitted diagnostics".into()
    };
    result
}

/// Digest-only capability: Guardian audit does not retain the source bytes.
pub fn capability(request: &PdfRequest) -> io::Result<sail_guardian::Capability> {
    request.validate()?;
    Ok(capability_for(
        &serde_json::to_vec(request).map_err(io::Error::other)?,
    ))
}

/// Authorize the exact input before opening the runner socket. Revocation and cancellation
/// both close the connection; the service kills the cgroup. Call from a blocking worker.
#[allow(clippy::too_many_arguments)]
pub fn dispatch(
    socket: &Path,
    runner_uid: u32,
    broker: &sail_guardian::GuardianExecutionBroker,
    request: &PdfRequest,
    input: &[u8],
    token: Option<Uuid>,
    cancel: CancellationToken,
) -> Result<io::Result<CommandResult>, sail_guardian::ExecutionDenied> {
    let denied = |error: io::Error| sail_guardian::ExecutionDenied::PolicyDenied {
        reason: error.to_string(),
    };
    request.validate().map_err(denied)?;
    validate_input(input).map_err(denied)?;
    if request.input_bytes != input.len() || request.input_sha256 != digest(input) {
        return Err(denied(io::Error::other("PDF input digest mismatch")));
    }
    let frame = serde_json::to_vec(request).map_err(|e| denied(io::Error::other(e)))?;
    let approvals = broker.approval().clone();
    broker.authorize_and_execute(
        SubsystemId::Arena,
        capability(request).map_err(denied)?,
        token,
        Some(request.correlation_id),
        || {
            exchange_input(
                socket,
                runner_uid,
                request.request_id,
                request.correlation_id,
                &frame,
                65_000,
                Arc::new(move || {
                    cancel.is_cancelled() || token.is_some_and(|t| approvals.is_revoked(t))
                }),
                Some(input),
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_requests_match_the_guardian_standing_pdf_rule() {
        let bytes = b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF";
        for operation in [
            PdfOperation::PageCount,
            PdfOperation::Pages { first: 1, last: 20 },
        ] {
            let request = PdfRequest::new(bytes, Uuid::new_v4(), operation).unwrap();
            let sail_guardian::Capability::Tool(sail_guardian::ToolCapability::SandboxCommand {
                request: json,
                ..
            }) = capability(&request).unwrap()
            else {
                panic!("PDF requests are sandbox commands");
            };
            assert_eq!(
                sail_guardian::policy::standing_sandbox_protocol(&json),
                Some(PROTOCOL),
                "{json}"
            );
        }
    }

    #[test]
    fn encryption_and_mismatched_frames_are_refused() {
        for bytes in [
            b"%PDF-1.7 /Encrypt 1 0 R".as_slice(),
            b"%PDF-1.7 /Encr#79pt 1 0 R",
        ] {
            assert_eq!(
                validate_input(bytes).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        assert!(validate_input(b"not a pdf").is_err());
        let mut request = PdfRequest::new(
            b"%PDF-1.7 /XRef /Prev",
            Uuid::new_v4(),
            PdfOperation::PageCount,
        )
        .unwrap();
        assert!(request.validate().is_ok());
        request.input_bytes = MAX_INPUT_BYTES + 1;
        assert!(request.validate().is_err());
    }
    #[tokio::test]
    async fn mismatch_encryption_and_same_uid_never_launch_a_parser() {
        let input = b"%PDF-1.7 synthetic";
        let request = PdfRequest::new(input, Uuid::new_v4(), PdfOperation::PageCount).unwrap();
        let uid = unsafe { libc::geteuid() };
        let config = RunnerConfig {
            rootfs: "/".into(),
            cgroup_parent: "/tmp".into(),
            runner_uid: uid,
            daemon_uid: uid,
        };
        let denied = execute(
            &config,
            &request,
            b"%PDF-1.7 changed".to_vec(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(denied.status, CommandStatus::Denied);
        let unavailable =
            execute(&config, &request, input.to_vec(), CancellationToken::new()).await;
        assert_eq!(unavailable.status, CommandStatus::Unavailable);
        let original = capability(&request).unwrap();
        let mut changed = request.clone();
        changed.input_sha256 = "a".repeat(64);
        assert_ne!(capability(&changed).unwrap(), original);
        changed = request.clone();
        changed.correlation_id = Uuid::new_v4();
        assert_ne!(capability(&changed).unwrap(), original);
    }
    #[test]
    fn range_and_hostile_frames_are_rejected_before_input_allocation() {
        let mut request =
            PdfRequest::new(b"%PDF-1.7", Uuid::new_v4(), PdfOperation::PageCount).unwrap();
        for (first, last) in [(0, 1), (1, 21), (5, 4), (1, u32::MAX)] {
            request.operation = PdfOperation::Pages { first, last };
            assert!(request.validate().is_err());
        }
        request.operation = PdfOperation::PageCount;
        request.input_bytes = MAX_INPUT_BYTES + 1;
        assert!(request.validate().is_err());
        let mut value = serde_json::to_value(&request).unwrap();
        value["input_bytes"] = 10.into();
        value["operation"] =
            serde_json::json!({"kind":"pages","first":1,"last":2,"argv":["/bin/sh"]});
        assert!(decode_frame(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    #[test]
    fn fixed_launch_has_no_user_controlled_paths_or_arguments() {
        let request = PdfRequest::new(
            b"%PDF-1.7 test",
            Uuid::new_v4(),
            PdfOperation::Pages { first: 1, last: 20 },
        )
        .unwrap();
        let spec = launch(&request, b"%PDF-1.7 test".to_vec());
        assert_eq!(spec.executable, "/usr/bin/pdftotext");
        assert_eq!(&spec.argv[spec.argv.len() - 2..], ["-", "-"]);
        assert_eq!(spec.limits.output_bytes / 2, MAX_TEXT_BYTES);
        assert_eq!(spec.limits.wall_ms, 30_000);
        assert!(matches!(
            decode_frame(&serde_json::to_vec(&request).unwrap()).unwrap(),
            Incoming::Pdf(_)
        ));
        let serialized = serde_json::to_string(&request).unwrap();
        assert!(!serialized.contains("%PDF"));
        let mut unknown: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        unknown["executable"] = "/bin/sh".into();
        assert!(decode_frame(&serde_json::to_vec(&unknown).unwrap()).is_err());
    }
}
