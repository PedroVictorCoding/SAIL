//! The GGUF header reader: synthetic headers, truncation, hostile counts and no panics.
use sail_mind::heavy::gguf::{GgufError, GgufTensor, parse, read_summary};
use std::io::Cursor;

const MAGIC: u32 = 0x4655_4747;
const ALIGNMENT: usize = 32;

fn push_str(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn meta_u32(bytes: &mut Vec<u8>, key: &str, value: u32) {
    push_str(bytes, key);
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn meta_str(bytes: &mut Vec<u8>, key: &str, value: &str) {
    push_str(bytes, key);
    bytes.extend_from_slice(&8u32.to_le_bytes());
    push_str(bytes, value);
}

fn meta_bool(bytes: &mut Vec<u8>, key: &str, value: bool) {
    push_str(bytes, key);
    bytes.extend_from_slice(&7u32.to_le_bytes());
    bytes.push(u8::from(value));
}

fn meta_f32(bytes: &mut Vec<u8>, key: &str, value: f32) {
    push_str(bytes, key);
    bytes.extend_from_slice(&6u32.to_le_bytes());
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn meta_array_u32(bytes: &mut Vec<u8>, key: &str, values: &[u32]) {
    push_str(bytes, key);
    bytes.extend_from_slice(&9u32.to_le_bytes());
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&(values.len() as u64).to_le_bytes());
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

fn meta_array_str(bytes: &mut Vec<u8>, key: &str, values: &[&str]) {
    push_str(bytes, key);
    bytes.extend_from_slice(&9u32.to_le_bytes());
    bytes.extend_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&(values.len() as u64).to_le_bytes());
    for value in values {
        push_str(bytes, value);
    }
}

fn tensor_info(bytes: &mut Vec<u8>, name: &str, offset: u64) {
    push_str(bytes, name);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&offset.to_le_bytes());
}

/// A valid synthetic GGUF. Returns the bytes and where the tensor data starts.
fn build(tensors: &[(&str, u64)], data_len: u64, architecture_last: bool) -> (Vec<u8>, usize) {
    let metadata = |bytes: &mut Vec<u8>| {
        meta_u32(bytes, "general.alignment", ALIGNMENT as u32);
        meta_str(bytes, "general.name", "synthetic");
        meta_bool(bytes, "synthetic.flag", true);
        meta_f32(bytes, "synthetic.scale", 1.5);
        meta_array_u32(bytes, "synthetic.numbers", &[1, 2, 3]);
        meta_array_str(bytes, "tokenizer.ggml.tokens", &["a", "bb"]);
        meta_u32(bytes, "gpt-oss.block_count", 36);
        meta_u32(bytes, "gpt-oss.expert_count", 128);
        meta_u32(bytes, "llama.block_count", 32);
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC.to_le_bytes());
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
    // alignment, name, flag, scale, numbers, tokens, block_count, expert_count,
    // llama.block_count, architecture
    bytes.extend_from_slice(&10u64.to_le_bytes());
    if !architecture_last {
        meta_str(&mut bytes, "general.architecture", "gpt-oss");
    }
    metadata(&mut bytes);
    if architecture_last {
        meta_str(&mut bytes, "general.architecture", "gpt-oss");
    }
    for (name, offset) in tensors {
        tensor_info(&mut bytes, name, *offset);
    }
    let padded = bytes.len().div_ceil(ALIGNMENT) * ALIGNMENT;
    bytes.resize(padded, 0);
    let data_start = bytes.len();
    bytes.resize(data_start + data_len as usize, 0xA5);
    (bytes, data_start)
}

fn parse_bytes(bytes: &[u8]) -> Result<sail_mind::heavy::gguf::GgufSummary, GgufError> {
    parse(Cursor::new(bytes.to_vec()), bytes.len() as u64)
}

#[test]
fn reads_metadata_and_tensor_sizes_sorted_by_offset() {
    let (bytes, _) = build(
        &[
            ("blk.2.ffn_down_exps.weight", 4096),
            ("blk.0.ffn_up_exps.weight", 0),
            ("blk.1.ffn_gate_exps.weight", 1024),
        ],
        8192,
        false,
    );
    let summary = parse_bytes(&bytes).expect("synthetic header");
    assert_eq!(summary.architecture, "gpt-oss");
    assert_eq!(summary.block_count, 36);
    assert_eq!(summary.expert_count, Some(128));
    assert_eq!(
        summary.tensors,
        vec![
            GgufTensor {
                name: "blk.0.ffn_up_exps.weight".into(),
                bytes: 1024
            },
            GgufTensor {
                name: "blk.1.ffn_gate_exps.weight".into(),
                bytes: 3072
            },
            GgufTensor {
                name: "blk.2.ffn_down_exps.weight".into(),
                bytes: 4096
            },
        ],
        "tensors are sorted by offset; the last runs to the end of the file"
    );

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("model.gguf");
    std::fs::write(&path, &bytes).expect("write model");
    assert_eq!(read_summary(&path).expect("read_summary"), summary);
}

#[test]
fn architecture_may_come_after_its_own_keys() {
    let (bytes, _) = build(&[("t", 0)], 64, true);
    let summary = parse_bytes(&bytes).expect("header with architecture last");
    assert_eq!(summary.architecture, "gpt-oss");
    assert_eq!(summary.block_count, 36);
    assert_eq!(summary.expert_count, Some(128));
}

#[test]
fn truncated_headers_are_refused_at_every_point() {
    let (bytes, data_start) = build(&[("a", 0), ("b", 64)], 256, false);
    let mut points: Vec<usize> = (0..data_start).step_by(7).collect();
    points.push(data_start - 1);
    for length in points {
        let error = parse_bytes(&bytes[..length]).expect_err("truncated header");
        assert!(
            matches!(error, GgufError::Truncated | GgufError::Malformed(_)),
            "length {length}: {error:?}"
        );
    }
}

#[test]
fn bad_magic_and_versions_are_reported() {
    let (mut bytes, _) = build(&[("a", 0)], 64, false);
    bytes[0] = b'X';
    assert_eq!(parse_bytes(&bytes), Err(GgufError::BadMagic));

    let (mut bytes, _) = build(&[("a", 0)], 64, false);
    bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(parse_bytes(&bytes), Err(GgufError::UnsupportedVersion(2)));
}

#[test]
fn absurd_counts_and_lengths_are_malformed_without_allocating() {
    let (mut bytes, _) = build(&[("a", 0)], 64, false);
    bytes[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(parse_bytes(&bytes), Err(GgufError::Malformed(_))));

    let (mut bytes, _) = build(&[("a", 0)], 64, false);
    bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(parse_bytes(&bytes), Err(GgufError::Malformed(_))));

    let mut header = Vec::new();
    header.extend_from_slice(&MAGIC.to_le_bytes());
    header.extend_from_slice(&3u32.to_le_bytes());
    header.extend_from_slice(&0u64.to_le_bytes());
    header.extend_from_slice(&1u64.to_le_bytes());
    header.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(parse_bytes(&header), Err(GgufError::Malformed(_))));

    let mut header = Vec::new();
    header.extend_from_slice(&MAGIC.to_le_bytes());
    header.extend_from_slice(&3u32.to_le_bytes());
    header.extend_from_slice(&0u64.to_le_bytes());
    header.extend_from_slice(&1u64.to_le_bytes());
    push_str(&mut header, "synthetic.array");
    header.extend_from_slice(&9u32.to_le_bytes());
    header.extend_from_slice(&4u32.to_le_bytes());
    header.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(parse_bytes(&header), Err(GgufError::Malformed(_))));
}

#[test]
fn a_missing_architecture_is_malformed_and_a_missing_file_is_io() {
    let mut header = Vec::new();
    header.extend_from_slice(&MAGIC.to_le_bytes());
    header.extend_from_slice(&3u32.to_le_bytes());
    header.extend_from_slice(&0u64.to_le_bytes());
    header.extend_from_slice(&0u64.to_le_bytes());
    assert!(matches!(parse_bytes(&header), Err(GgufError::Malformed(_))));

    let dir = tempfile::tempdir().expect("temp dir");
    assert!(matches!(
        read_summary(&dir.path().join("missing.gguf")),
        Err(GgufError::Io(_))
    ));
}

#[test]
fn hostile_bytes_never_panic() {
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for length in 0..512usize {
        let bytes: Vec<u8> = (0..length).map(|_| next() as u8).collect();
        let _ = parse_bytes(&bytes);
    }
    for length in 0..512usize {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&(next() % 4).to_le_bytes());
        bytes.extend_from_slice(&(next() % 4).to_le_bytes());
        bytes.extend((0..length).map(|_| next() as u8));
        let _ = parse_bytes(&bytes);
    }
}
