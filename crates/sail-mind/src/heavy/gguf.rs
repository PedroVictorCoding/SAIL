//! A streaming GGUF v3 header reader: metadata and tensor offsets only, never tensor data.
use std::{
    io::{Read, Seek},
    path::Path,
};

/// One tensor as the header describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufTensor {
    pub name: String,
    pub bytes: u64,
}

/// The header information placement needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufSummary {
    pub architecture: String,
    pub block_count: u32,
    pub expert_count: Option<u32>,
    pub tensors: Vec<GgufTensor>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GgufError {
    #[error("cannot read the model file: {0}")]
    Io(String),
    #[error("not a GGUF file")]
    BadMagic,
    #[error("unsupported GGUF version {0}")]
    UnsupportedVersion(u32),
    #[error("the GGUF header is truncated")]
    Truncated,
    #[error("malformed GGUF header: {0}")]
    Malformed(String),
}

/// Read the header of the GGUF at `path`.
pub fn read_summary(path: &Path) -> Result<GgufSummary, GgufError> {
    Err(GgufError::Io(format!(
        "reading {} is not implemented yet",
        path.display()
    )))
}

/// Read a GGUF v3 header from `reader`; `file_len` bounds every offset and length.
pub fn parse<R: Read + Seek>(_reader: R, _file_len: u64) -> Result<GgufSummary, GgufError> {
    Err(GgufError::Io(
        "the GGUF header reader is not implemented yet".into(),
    ))
}
