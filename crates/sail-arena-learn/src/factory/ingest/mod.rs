//! Pure, bounded extraction. Source material stays data and is never executed.
use super::contracts::{
    FragmentKind, IngestionStatus, QualityStatus, SourceFragment, SourceLocation, SourceType,
    sha256_hex,
};

pub mod epub;
pub mod pdf;
pub mod text;
mod zip;

/// Reject large uploads before a parser allocates from their contents.
pub const MAX_FILE_BYTES: usize = 256 * 1024 * 1024;
/// Limit extraction output independently of compressed input size.
pub const MAX_EXTRACTED_BYTES: usize = 64 * 1024 * 1024;
/// Bound per-source indexing and downstream model retrieval work.
pub const MAX_FRAGMENTS: usize = 20_000;
/// Keep individual fragments suitable for bounded authoring contexts.
pub const MAX_FRAGMENT_BYTES: usize = 4096;
/// Bound hostile document trees before traversing them.
pub const MAX_DEPTH: usize = 32;
/// Bound document page/spine counts independently of file bytes.
pub const MAX_PAGES: usize = 5000;

/// Extraction errors describe rejected inputs without including private source text.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// Malformed documents never become silently empty indexed sources.
    #[error("invalid source: {0}")]
    Invalid(String),
    /// Resource limits apply to expanded contents as well as uploaded bytes.
    #[error("source exceeds {0} limit")]
    Limit(&'static str),
    /// Encrypted content is refused; ingestion never decrypts it.
    #[error("DRM/encryption refused: {0}")]
    Drm(String),
}

/// Extracted fragments and the honest source-level readiness state.
#[derive(Debug)]
pub struct Extracted {
    /// Identifies scanned PDFs without pretending OCR exists.
    pub source_type: SourceType,
    /// Scanned or incomplete sources need owner review.
    pub status: IngestionStatus,
    /// Reading-order fragments whose identities include source revision.
    pub fragments: Vec<SourceFragment>,
}

/// Dispatch by explicit filename extension; no subprocess, model, filesystem or network IO.
pub fn extract(
    file_name: &str,
    bytes: &[u8],
    source_id: &str,
    revision: u64,
) -> Result<Extracted, IngestError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(IngestError::Limit("file bytes"));
    }
    if source_id.len() > 256 || revision == 0 {
        return Err(IngestError::Invalid("invalid source identity".into()));
    }
    let ext = file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => pdf::extract(bytes, source_id, revision),
        "epub" => epub::extract(bytes, source_id, revision),
        "md" | "markdown" => text::extract(bytes, source_id, revision, true),
        "txt" | "text" => text::extract(bytes, source_id, revision, false),
        _ => Err(IngestError::Invalid(
            "supported formats: PDF, EPUB, Markdown, text".into(),
        )),
    }
}

pub(crate) struct Builder {
    pub fragments: Vec<SourceFragment>,
    bytes: usize,
    source: String,
    revision: u64,
}
impl Builder {
    pub fn new(source: &str, revision: u64) -> Self {
        Self {
            fragments: Vec::new(),
            bytes: 0,
            source: source.into(),
            revision,
        }
    }
    pub fn push(
        &mut self,
        text: &str,
        kind: FragmentKind,
        path: &[String],
        location: SourceLocation,
        quality: QualityStatus,
    ) -> Result<(), IngestError> {
        if path.len() > MAX_DEPTH || path.iter().any(|part| part.len() > MAX_FRAGMENT_BYTES) {
            return Err(IngestError::Limit("chapter path"));
        }
        let path_bytes: usize = path.iter().map(String::len).sum();
        let location_bytes = serde_json::to_vec(&location)
            .map_err(|_| IngestError::Invalid("fragment location".into()))?
            .len();
        let mut remaining = text.trim();
        while !remaining.is_empty() {
            if self.fragments.len() >= MAX_FRAGMENTS {
                return Err(IngestError::Limit("fragments"));
            }
            let mut end = remaining.len().min(MAX_FRAGMENT_BYTES);
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            if end < remaining.len() {
                let prefix = &remaining[..end];
                if let Some(pos) = prefix.rfind("\n\n").filter(|p| *p > end / 2) {
                    end = pos + 2;
                } else if let Some(pos) = prefix.rfind(char::is_whitespace).filter(|p| *p > end / 2)
                {
                    end = pos + 1;
                }
            }
            let chunk = remaining[..end].trim();
            self.bytes += chunk.len() + path_bytes + location_bytes + self.source.len() + 256;
            if self.bytes > MAX_EXTRACTED_BYTES {
                return Err(IngestError::Limit("extracted bytes"));
            }
            let content_hash = sha256_hex(chunk.as_bytes());
            let identity = serde_json::to_vec(&(
                &self.source,
                self.revision,
                &location,
                self.fragments.len(),
                &content_hash,
            ))
            .map_err(|_| IngestError::Invalid("fragment identity".into()))?;
            self.fragments.push(SourceFragment {
                fragment_id: sha256_hex(&identity),
                source_id: self.source.clone(),
                source_revision: self.revision,
                kind,
                chapter_path: path.to_vec(),
                location: location.clone(),
                text: chunk.into(),
                content_hash,
                quality: quality.clone(),
            });
            remaining = &remaining[end..];
        }
        Ok(())
    }
}

pub(crate) fn classify(text: &str) -> FragmentKind {
    let lower: String = text.chars().take(80).flat_map(char::to_lowercase).collect();
    for (prefix, kind) in [
        ("definition", FragmentKind::Definition),
        ("theorem", FragmentKind::Theorem),
        ("example", FragmentKind::Example),
        ("exercise", FragmentKind::Exercise),
        ("solution", FragmentKind::Solution),
        ("listing", FragmentKind::Code),
        ("figure", FragmentKind::Figure),
        ("bibliography", FragmentKind::Bibliography),
        ("references", FragmentKind::Bibliography),
    ] {
        if lower.starts_with(prefix) {
            return kind;
        }
    }
    if lower.starts_with("$$") || lower.starts_with("\\[") {
        FragmentKind::Formula
    } else {
        FragmentKind::Prose
    }
}

pub(crate) fn indexed(source_type: SourceType, fragments: Vec<SourceFragment>) -> Extracted {
    Extracted {
        source_type,
        status: IngestionStatus::Indexed,
        fragments,
    }
}
