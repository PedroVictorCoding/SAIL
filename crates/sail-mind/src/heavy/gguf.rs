//! A streaming GGUF v3 header reader: metadata and tensor offsets only, never tensor data.
//!
//! Only the header and the tensor-info region are read. A hostile header cannot make the
//! reader allocate gigabytes: counts, strings, arrays and the total header bytes are capped.
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// GGUF magic, little-endian: the bytes `GGUF`.
const GGUF_MAGIC: u32 = 0x4655_4747;
/// The only header version this reader accepts.
const GGUF_VERSION: u32 = 3;
/// Metadata default alignment when the file does not say otherwise.
const DEFAULT_ALIGNMENT: u64 = 32;
/// Most tensors a header may declare. Fixed a priori: the largest real models are below this.
const MAX_TENSORS: u64 = 65_536;
/// Most metadata entries a header may declare. Fixed a priori.
const MAX_METADATA_KEYS: u64 = 65_536;
/// Longest string value or tensor name accepted. Fixed a priori (1 MiB).
const MAX_STRING_BYTES: u64 = 1 << 20;
/// Longest metadata key accepted. Fixed a priori: real keys are short.
const MAX_KEY_BYTES: u64 = 1024;
/// Most elements an array may declare. Fixed a priori (16 Mi elements).
const MAX_ARRAY_ELEMENTS: u64 = 16 << 20;
/// Most bytes the reader will take from the header region. Fixed a priori (64 MiB): the
/// tensor-info region of a 120B model is a few MiB.
const MAX_HEADER_BYTES: u64 = 64 << 20;
/// Deepest nested array accepted. Fixed a priori: GGUF arrays are flat in real files.
const MAX_ARRAY_DEPTH: u8 = 8;
/// Maximum tensor dimensions. Fixed a priori: ggml defines at most four.
const MAX_DIMS: u32 = 4;

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
    let file_len = std::fs::metadata(path)
        .map_err(|error| GgufError::Io(error.to_string()))?
        .len();
    let file = std::fs::File::open(path).map_err(|error| GgufError::Io(error.to_string()))?;
    parse(std::io::BufReader::new(file), file_len)
}

/// Read a GGUF v3 header from `reader`; `file_len` bounds every offset and length.
pub fn parse<R: Read + Seek>(mut reader: R, file_len: u64) -> Result<GgufSummary, GgufError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| GgufError::Io(error.to_string()))?;
    let mut reader = Header::new(reader, file_len);

    if reader.u32()? != GGUF_MAGIC {
        return Err(GgufError::BadMagic);
    }
    let version = reader.u32()?;
    if version != GGUF_VERSION {
        return Err(GgufError::UnsupportedVersion(version));
    }
    let tensor_count = reader.u64()?;
    if tensor_count > MAX_TENSORS {
        return Err(GgufError::Malformed(format!(
            "{tensor_count} tensors is more than {MAX_TENSORS}"
        )));
    }
    let metadata_count = reader.u64()?;
    if metadata_count > MAX_METADATA_KEYS {
        return Err(GgufError::Malformed(format!(
            "{metadata_count} metadata entries is more than {MAX_METADATA_KEYS}"
        )));
    }

    let mut architecture: Option<String> = None;
    let mut alignment = DEFAULT_ALIGNMENT;
    // Architecture-specific keys may come before `general.architecture`; keep only the two
    // counts placement needs, keyed by the full name.
    let mut counts: HashMap<String, u32> = HashMap::new();
    for _ in 0..metadata_count {
        let key = reader.string(MAX_KEY_BYTES)?;
        let kind = reader.u32()?;
        match kind {
            0 | 1 => {
                reader.skip(1)?;
            }
            2 | 3 => {
                reader.skip(2)?;
            }
            4 => {
                let value = reader.u32()?;
                if key == "general.alignment" {
                    alignment = u64::from(value);
                }
                remember_count(&mut counts, &key, value);
            }
            5 | 6 => {
                reader.skip(4)?;
            }
            7 => {
                reader.skip(1)?;
            }
            8 => {
                let value = reader.string(MAX_STRING_BYTES)?;
                if key == "general.architecture" {
                    architecture = Some(value);
                }
            }
            9 => reader.skip_array(0)?,
            10 => {
                let value = reader.u64()?;
                if value <= u64::from(u32::MAX) {
                    remember_count(&mut counts, &key, value as u32);
                }
            }
            11 | 12 => {
                reader.skip(8)?;
            }
            other => {
                return Err(GgufError::Malformed(format!(
                    "unknown metadata type {other}"
                )));
            }
        }
    }

    let architecture = architecture
        .ok_or_else(|| GgufError::Malformed("general.architecture is missing".into()))?;
    let block_count = counts
        .get(&format!("{architecture}.block_count"))
        .copied()
        .unwrap_or(0);
    let expert_count = counts.get(&format!("{architecture}.expert_count")).copied();

    let mut infos = Vec::with_capacity(tensor_count as usize);
    for _ in 0..tensor_count {
        let name = reader.string(MAX_STRING_BYTES)?;
        let dims = reader.u32()?;
        if dims > MAX_DIMS {
            return Err(GgufError::Malformed(format!(
                "tensor {name:?} has {dims} dimensions (at most {MAX_DIMS})"
            )));
        }
        for _ in 0..dims {
            reader.u64()?;
        }
        let _ggml_type = reader.u32()?;
        let offset = reader.u64()?;
        infos.push((name, offset));
    }

    let header_end = reader.position();
    let aligned = if alignment == 0 || !alignment.is_power_of_two() {
        DEFAULT_ALIGNMENT
    } else {
        alignment
    };
    let data_start = align_up(header_end, aligned)
        .ok_or_else(|| GgufError::Malformed("header end overflows the file".into()))?;
    let data_len = file_len
        .checked_sub(data_start)
        .ok_or(GgufError::Truncated)?;

    infos.sort_by(|(left_name, left), (right_name, right)| {
        left.cmp(right).then_with(|| left_name.cmp(right_name))
    });
    let mut tensors = Vec::with_capacity(infos.len());
    for (index, (name, offset)) in infos.iter().enumerate() {
        let end = infos
            .get(index + 1)
            .map(|(_, next)| *next)
            .unwrap_or(data_len);
        if *offset > data_len || end < *offset {
            return Err(GgufError::Truncated);
        }
        tensors.push(GgufTensor {
            name: name.clone(),
            bytes: end - *offset,
        });
    }

    Ok(GgufSummary {
        architecture,
        block_count,
        expert_count,
        tensors,
    })
}

fn remember_count(counts: &mut HashMap<String, u32>, key: &str, value: u32) {
    if key.ends_with(".block_count") || key.ends_with(".expert_count") {
        counts.insert(key.to_owned(), value);
    }
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    let remainder = value % alignment;
    if remainder == 0 {
        Some(value)
    } else {
        value.checked_add(alignment - remainder)
    }
}

/// The header reader with position, length and header-budget tracking.
struct Header<R> {
    inner: R,
    position: u64,
    file_len: u64,
    consumed: u64,
}

impl<R: Read> Header<R> {
    fn new(inner: R, file_len: u64) -> Self {
        Self {
            inner,
            position: 0,
            file_len,
            consumed: 0,
        }
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn read_exact(&mut self, len: usize) -> Result<Vec<u8>, GgufError> {
        let len = u64::try_from(len)
            .map_err(|_| GgufError::Malformed("a length does not fit this machine".into()))?;
        self.take(len)?;
        let mut bytes = vec![0u8; len as usize];
        self.inner.read_exact(&mut bytes).map_err(read_error)?;
        self.position += len;
        self.consumed += len;
        Ok(bytes)
    }

    /// Advance past `len` bytes without keeping them.
    fn skip(&mut self, len: u64) -> Result<(), GgufError> {
        self.take(len)?;
        let mut buffer = [0u8; 8 * 1024];
        let mut left = len;
        while left > 0 {
            let chunk = usize::try_from(left.min(buffer.len() as u64))
                .map_err(|_| GgufError::Malformed("a length does not fit this machine".into()))?;
            self.inner
                .read_exact(&mut buffer[..chunk])
                .map_err(read_error)?;
            left -= chunk as u64;
            self.position += chunk as u64;
            self.consumed += chunk as u64;
        }
        Ok(())
    }

    /// Check that `len` bytes are inside the file and the header budget.
    fn take(&self, len: u64) -> Result<(), GgufError> {
        if len > self.file_len.saturating_sub(self.position) {
            return Err(GgufError::Truncated);
        }
        if len > MAX_HEADER_BYTES.saturating_sub(self.consumed) {
            return Err(GgufError::Malformed(
                "the header is larger than the reader allows".into(),
            ));
        }
        Ok(())
    }

    fn u32(&mut self) -> Result<u32, GgufError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, GgufError> {
        let bytes = self.read_exact(8)?;
        let array: [u8; 8] = bytes
            .try_into()
            .map_err(|_| GgufError::Malformed("short u64".into()))?;
        Ok(u64::from_le_bytes(array))
    }

    fn string(&mut self, max_len: u64) -> Result<String, GgufError> {
        let len = self.u64()?;
        if len > max_len {
            return Err(GgufError::Malformed(format!(
                "a string of {len} bytes is longer than {max_len}"
            )));
        }
        let len = usize::try_from(len)
            .map_err(|_| GgufError::Malformed("a string does not fit this machine".into()))?;
        let bytes = self.read_exact(len)?;
        String::from_utf8(bytes)
            .map_err(|_| GgufError::Malformed("a string is not valid UTF-8".into()))
    }

    /// Skip one array value; `depth` bounds nested arrays.
    fn skip_array(&mut self, depth: u8) -> Result<(), GgufError> {
        if depth >= MAX_ARRAY_DEPTH {
            return Err(GgufError::Malformed("arrays are nested too deeply".into()));
        }
        let kind = self.u32()?;
        let count = self.u64()?;
        if count > MAX_ARRAY_ELEMENTS {
            return Err(GgufError::Malformed(format!(
                "an array of {count} elements is larger than {MAX_ARRAY_ELEMENTS}"
            )));
        }
        match kind {
            8 => {
                for _ in 0..count {
                    let len = self.u64()?;
                    if len > MAX_STRING_BYTES {
                        return Err(GgufError::Malformed(format!(
                            "an array string of {len} bytes is longer than {MAX_STRING_BYTES}"
                        )));
                    }
                    self.skip(len)?;
                }
                Ok(())
            }
            9 => {
                for _ in 0..count {
                    self.skip_array(depth + 1)?;
                }
                Ok(())
            }
            other => {
                let size = fixed_size(other)?;
                let total = count
                    .checked_mul(size)
                    .ok_or_else(|| GgufError::Malformed("an array length overflows".into()))?;
                self.skip(total)
            }
        }
    }
}

fn fixed_size(kind: u32) -> Result<u64, GgufError> {
    match kind {
        0 | 1 | 7 => Ok(1),
        2 | 3 => Ok(2),
        4..=6 => Ok(4),
        10..=12 => Ok(8),
        other => Err(GgufError::Malformed(format!(
            "unknown metadata type {other}"
        ))),
    }
}

fn read_error(error: std::io::Error) -> GgufError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        GgufError::Truncated
    } else {
        GgufError::Io(error.to_string())
    }
}
