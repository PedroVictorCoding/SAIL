/// A self-contained evaluation fixture representing a realistic SAIL repository task.
#[derive(Debug, Clone)]
pub struct IssueFixture {
    pub id: u64,
    pub title: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub difficulty: &'static str,
    pub initial_files: Vec<(&'static str, &'static str)>,
    pub reference_patch: &'static str,
    pub test_code: &'static str,
}

/// Retrieve the 8 representative SAIL benchmark issue fixtures.
pub fn get_benchmark_fixtures() -> Vec<IssueFixture> {
    vec![
        // 1. Rust compile fix: borrow checker / mutability error
        IssueFixture {
            id: 101,
            title: "Fix mutable borrow collision in cache eviction buffer",
            description: "The cache eviction logic fails to compile with E0502: cannot borrow self.items as mutable because it is also borrowed as immutable. Refactor to split borrows cleanly.",
            category: "compile_fix",
            difficulty: "easy",
            initial_files: vec![(
                "src/cache.rs",
                r##"pub struct Cache {
    pub items: Vec<(String, u64)>,
}

impl Cache {
    pub fn new() -> Self { Self { items: Vec::new() } }
    pub fn push(&mut self, key: String, val: u64) { self.items.push((key, val)); }
    pub fn evict_oldest(&mut self) -> Option<(String, u64)> {
        if self.items.is_empty() { return None; }
        let oldest = self.items.iter().min_by_key(|(_, val)| *val)?;
        let key = oldest.0.clone();
        let idx = self.items.iter().position(|(k, _)| k == &key)?;
        Some(self.items.remove(idx))
    }
}
"##,
            )],
            reference_patch: r##"--- a/src/cache.rs
+++ b/src/cache.rs
@@ -10,3 +10,3 @@
-        let oldest = self.items.iter().min_by_key(|(_, val)| *val)?;
-        let key = oldest.0.clone();
-        let idx = self.items.iter().position(|(k, _)| k == &key)?;
+        let idx = self.items.iter().enumerate().min_by_key(|(_, (_, val))| *val)?.0;
         Some(self.items.remove(idx))
"##,
            test_code: r##"
#[test]
fn test_cache_evict() {
    let mut c = Cache::new();
    c.push("a".into(), 10);
    c.push("b".into(), 5);
    c.push("c".into(), 20);
    let evicted = c.evict_oldest();
    assert_eq!(evicted, Some(("b".into(), 5)));
    assert_eq!(c.items.len(), 2);
}
"##,
        },
        // 2. Bugfix: off-by-one slice indexing boundary
        IssueFixture {
            id: 102,
            title: "Fix off-by-one index panic in token chunking parser",
            description: "When splitting tokens by maximum chunk limit, slices ending precisely at chunk boundary panic with index out of bounds.",
            category: "bugfix",
            difficulty: "easy",
            initial_files: vec![(
                "src/chunker.rs",
                r##"pub fn chunk_tokens(tokens: &[u32], chunk_size: usize) -> Vec<Vec<u32>> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < tokens.len() {
        let end = (start + chunk_size).min(tokens.len());
        chunks.push(tokens[start..end].to_vec());
        start += chunk_size;
    }
    chunks
}
"##,
            )],
            reference_patch: r##"--- a/src/chunker.rs
+++ b/src/chunker.rs
@@ -4,4 +4,4 @@
     while start < tokens.len() {
-        let end = (start + chunk_size).min(tokens.len());
+        let end = (start + chunk_size).min(tokens.len());
         chunks.push(tokens[start..end].to_vec());
         start += chunk_size;
"##,
            test_code: r##"
#[test]
fn test_chunk_tokens_exact_boundary() {
    let tokens = vec![1, 2, 3, 4, 5, 6];
    let chunks = chunk_tokens(&tokens, 3);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0], vec![1, 2, 3]);
    assert_eq!(chunks[1], vec![4, 5, 6]);
}
"##,
        },
        // 3. Test addition: regression coverage for empty string input
        IssueFixture {
            id: 103,
            title: "Add regression test suite for empty and unicode prompt inputs",
            description: "Tokenizer sanitizer lacks unit tests verifying zero-length input strings and multi-byte UTF-8 graphemes.",
            category: "tests",
            difficulty: "easy",
            initial_files: vec![(
                "src/sanitizer.rs",
                r##"pub fn sanitize_prompt(input: &str) -> String {
    input.trim().replace('\0', "")
}
"##,
            )],
            reference_patch: r##"--- a/src/sanitizer.rs
+++ b/src/sanitizer.rs
@@ -4,0 +5,10 @@
+#[cfg(test)]
+mod tests {
+    use super::*;
+    #[test]
+    fn test_empty_and_unicode() {
+        assert_eq!(sanitize_prompt(""), "");
+        assert_eq!(sanitize_prompt("   "), "");
+        assert_eq!(sanitize_prompt("  🦀 hello 🚀  "), "🦀 hello 🚀");
+    }
+}
"##,
            test_code: r##"
#[test]
fn test_sanitizer_edge_cases() {
    assert_eq!(sanitize_prompt(""), "");
    assert_eq!(sanitize_prompt("   "), "");
    assert_eq!(sanitize_prompt("  🦀 hello 🚀  "), "🦀 hello 🚀");
}
"##,
        },
        // 4. Protocol contract change: add backward-compatible optional field
        IssueFixture {
            id: 104,
            title: "Extend InferenceRequest with optional priority and trace_id fields",
            description: "Add priority and trace_id to InferenceRequest contract with default None for backward compatibility.",
            category: "protocol",
            difficulty: "medium",
            initial_files: vec![(
                "src/protocol.rs",
                r##"use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceRequest {
    pub prompt: String,
    pub max_tokens: u32,
}
"##,
            )],
            reference_patch: r##"--- a/src/protocol.rs
+++ b/src/protocol.rs
@@ -5,2 +5,6 @@
     pub prompt: String,
     pub max_tokens: u32,
+    #[serde(default, skip_serializing_if = "Option::is_none")]
+    pub priority: Option<u8>,
+    #[serde(default, skip_serializing_if = "Option::is_none")]
+    pub trace_id: Option<String>,
"##,
            test_code: r##"
#[test]
fn test_protocol_backward_compatibility() {
    let legacy_json = r#"{"prompt":"hello","max_tokens":128}"#;
    let parsed: InferenceRequest = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(parsed.priority, None);
    assert_eq!(parsed.trace_id, None);
}
"##,
        },
        // 5. Small feature: CLI flag --dry-run and --format json
        IssueFixture {
            id: 105,
            title: "Implement dry-run mode and format switch for runner CLI",
            description: "Add --dry-run and --format flags to the execution runner to enable safe preview without committing changes.",
            category: "feature",
            difficulty: "medium",
            initial_files: vec![(
                "src/cli.rs",
                r##"#[derive(Debug, Clone, PartialEq)]
pub struct CliOptions {
    pub target: String,
    pub dry_run: bool,
    pub format: String,
}

impl CliOptions {
    pub fn parse(args: &[String]) -> Self {
        let mut target = "default".to_string();
        let mut dry_run = false;
        let mut format = "text".to_string();
        for arg in args {
            if arg == "--dry-run" {
                dry_run = true;
            } else if let Some(fmt) = arg.strip_prefix("--format=") {
                format = fmt.to_string();
            } else if !arg.starts_with("--") {
                target = arg.clone();
            }
        }
        Self { target, dry_run, format }
    }
}
"##,
            )],
            reference_patch: r##"--- a/src/cli.rs
+++ b/src/cli.rs
@@ -17,2 +17,4 @@
             } else if let Some(fmt) = arg.strip_prefix("--format=") {
                 format = fmt.to_string();
+            } else if arg == "--json" {
+                format = "json".to_string();
"##,
            test_code: r##"
#[test]
fn test_cli_flags() {
    let args = vec!["--dry-run".into(), "--format=json".into(), "test_target".into()];
    let opts = CliOptions::parse(&args);
    assert!(opts.dry_run);
    assert_eq!(opts.format, "json");
    assert_eq!(opts.target, "test_target");
}
"##,
        },
        // 6. Error handling: Replace panic/unwrap with structured Result
        IssueFixture {
            id: 106,
            title: "Replace unchecked unwrap in audio sample rate converter",
            description: "Function resample_pcm calls unwrap on unsupported sample rates, causing daemon panic. Return Result<Vec<f32>, ResampleError>.",
            category: "error_handling",
            difficulty: "easy",
            initial_files: vec![(
                "src/resample.rs",
                r##"#[derive(Debug, PartialEq, thiserror::Error)]
pub enum ResampleError {
    #[error("Unsupported rate {0}")]
    UnsupportedRate(u32),
}

pub fn resample_pcm(input: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>, ResampleError> {
    if from_rate == 0 || to_rate == 0 {
        return Err(ResampleError::UnsupportedRate(0));
    }
    if from_rate == to_rate {
        return Ok(input.to_vec());
    }
    let ratio = to_rate as f32 / from_rate as f32;
    let mut out = Vec::new();
    for &sample in input {
        out.push(sample * ratio);
    }
    Ok(out)
}
"##,
            )],
            reference_patch: r##"--- a/src/resample.rs
+++ b/src/resample.rs
@@ -8,2 +8,4 @@
     if from_rate == 0 || to_rate == 0 {
         return Err(ResampleError::UnsupportedRate(0));
     }
"##,
            test_code: r##"
#[test]
fn test_resample_zero_rate_returns_err() {
    let data = vec![0.1, 0.2];
    assert!(resample_pcm(&data, 0, 16000).is_err());
    assert!(resample_pcm(&data, 16000, 16000).is_ok());
}
"##,
        },
        // 7. Concurrency / async fix: Prevent mutex lock order inversion
        IssueFixture {
            id: 107,
            title: "Prevent lock inversion in duplex stream session",
            description: "Locking session_id before buffer in write while read locks buffer before session_id causes deadlock under load.",
            category: "concurrency",
            difficulty: "medium",
            initial_files: vec![(
                "src/duplex.rs",
                r##"use std::sync::Mutex;

pub struct DuplexSession {
    pub session_id: Mutex<String>,
    pub buffer: Mutex<Vec<u8>>,
}

impl DuplexSession {
    pub fn new(id: &str) -> Self {
        Self {
            session_id: Mutex::new(id.to_string()),
            buffer: Mutex::new(Vec::new()),
        }
    }

    pub fn append(&self, bytes: &[u8]) {
        let _id = self.session_id.lock().unwrap();
        let mut buf = self.buffer.lock().unwrap();
        buf.extend_from_slice(bytes);
    }

    pub fn read_all(&self) -> Vec<u8> {
        let _id = self.session_id.lock().unwrap();
        let buf = self.buffer.lock().unwrap();
        buf.clone()
    }
}
"##,
            )],
            reference_patch: r##"--- a/src/duplex.rs
+++ b/src/duplex.rs
@@ -19,2 +19,2 @@
     pub fn read_all(&self) -> Vec<u8> {
         let _id = self.session_id.lock().unwrap();
"##,
            test_code: r##"
#[test]
fn test_duplex_session_concurrency() {
    let s = std::sync::Arc::new(DuplexSession::new("test"));
    s.append(b"hello");
    assert_eq!(s.read_all(), b"hello");
}
"##,
        },
        // 8. Refactoring: Clippy lint cleanup and redundant clone removal
        IssueFixture {
            id: 108,
            title: "Eliminate redundant clones in EventEnvelope dispatch",
            description: "Remove unnecessary clone on non-mutated String and Vec references in EventEnvelope builder.",
            category: "refactoring",
            difficulty: "easy",
            initial_files: vec![(
                "src/envelope.rs",
                r##"#[derive(Debug, Clone, PartialEq)]
pub struct EventEnvelope {
    pub topic: String,
    pub payload: String,
}

impl EventEnvelope {
    pub fn new(topic: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            payload: payload.into(),
        }
    }

    pub fn matches_topic(&self, prefix: &str) -> bool {
        self.topic.starts_with(prefix)
    }
}
"##,
            )],
            reference_patch: r##"--- a/src/envelope.rs
+++ b/src/envelope.rs
@@ -14,2 +14,2 @@
     pub fn matches_topic(&self, prefix: &str) -> bool {
         self.topic.starts_with(prefix)
"##,
            test_code: r##"
#[test]
fn test_envelope_matches() {
    let env = EventEnvelope::new("sail:forge:issue", "{}");
    assert!(env.matches_topic("sail:forge"));
    assert!(!env.matches_topic("sail:mind"));
}
"##,
        },
    ]
}

/// Deterministic 2-file offline Rust crate fixture for Increment A smoke verification:
/// - 2 source files (`src/lib.rs` and `src/pricing.rs`) with an initial failing unit test
/// - 1 reference 2-file patch that fixes the bug and makes `cargo test --offline` pass
/// - 1 deliberately broken patch that fails `cargo test --offline`
/// - 1 malicious `build.rs` probe that attempts host/network/secret escapes inside Bubblewrap
#[derive(Debug, Clone)]
pub struct MultiFileSmokeFixture {
    pub id: u64,
    pub title: &'static str,
    pub description: &'static str,
    pub cargo_toml: &'static str,
    pub initial_files: Vec<(&'static str, &'static str)>,
    pub reference_patch: &'static str,
    pub broken_patch: &'static str,
    pub malicious_build_rs: &'static str,
}

pub fn get_multi_file_smoke_fixture() -> MultiFileSmokeFixture {
    MultiFileSmokeFixture {
        id: 12201,
        title: "Apply basis-point fee discount across invoice line items and subtotal",
        description: "Invoice::total_cents in src/lib.rs ignores Invoice::discount_bps when calling pricing::apply_discount_bps in src/pricing.rs, and pricing::apply_discount_bps rounds in the wrong direction by adding instead of subtracting the discount.",
        cargo_toml: r#"[package]
name = "smoke_invoice_fixture"
version = "0.1.0"
edition = "2021"
"#,
        initial_files: vec![
            (
                "src/pricing.rs",
                r#"/// Apply a discount in basis points (100 bps = 1%) to a cent amount.
pub fn apply_discount_bps(amount_cents: u64, discount_bps: u32) -> u64 {
    let clamped_bps = discount_bps.min(10_000) as u64;
    let discount = (amount_cents * clamped_bps) / 10_000;
    amount_cents + discount
}
"#,
            ),
            (
                "src/lib.rs",
                r#"pub mod pricing;

pub struct Invoice {
    pub items_cents: Vec<u64>,
    pub discount_bps: u32,
}

impl Invoice {
    pub fn new(items_cents: Vec<u64>, discount_bps: u32) -> Self {
        Self {
            items_cents,
            discount_bps,
        }
    }

    pub fn subtotal_cents(&self) -> u64 {
        self.items_cents.iter().copied().sum()
    }

    pub fn total_cents(&self) -> u64 {
        let subtotal = self.subtotal_cents();
        pricing::apply_discount_bps(subtotal, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invoice_discount_calculation() {
        let inv = Invoice::new(vec![1200, 800], 1500); // $20.00 with 15% discount -> $17.00 (1700 cents)
        assert_eq!(inv.subtotal_cents(), 2000);
        assert_eq!(inv.total_cents(), 1700);
    }
}
"#,
            ),
        ],
        reference_patch: r#"--- a/src/pricing.rs
+++ b/src/pricing.rs
@@ -2,5 +2,5 @@
 pub fn apply_discount_bps(amount_cents: u64, discount_bps: u32) -> u64 {
     let clamped_bps = discount_bps.min(10_000) as u64;
     let discount = (amount_cents * clamped_bps) / 10_000;
-    amount_cents + discount
+    amount_cents.saturating_sub(discount)
 }
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -19,4 +19,4 @@
     pub fn total_cents(&self) -> u64 {
         let subtotal = self.subtotal_cents();
-        pricing::apply_discount_bps(subtotal, 0)
+        pricing::apply_discount_bps(subtotal, self.discount_bps)
     }
"#,
        broken_patch: r#"--- a/src/pricing.rs
+++ b/src/pricing.rs
@@ -2,5 +2,5 @@
 pub fn apply_discount_bps(amount_cents: u64, discount_bps: u32) -> u64 {
     let clamped_bps = discount_bps.min(10_000) as u64;
     let discount = (amount_cents * clamped_bps) / 10_000;
-    amount_cents + discount
+    amount_cents.saturating_sub(discount * 2)
 }
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -19,4 +19,4 @@
     pub fn total_cents(&self) -> u64 {
         let subtotal = self.subtotal_cents();
-        pricing::apply_discount_bps(subtotal, 0)
+        pricing::apply_discount_bps(subtotal, self.discount_bps)
     }
"#,
        malicious_build_rs: r#"use std::fs;
use std::net::TcpStream;
use std::time::Duration;

fn main() {
    // 1. Attempt to write outside worktree into /home/main
    if fs::write("/home/main/pwned_by_build_rs", "escaped").is_ok() {
        panic!("SANDBOX ESCAPE: wrote to /home/main/pwned_by_build_rs");
    }
    // 2. Attempt to read host SSH keys
    if fs::read_to_string("/home/main/.ssh/id_rsa").is_ok() {
        panic!("SANDBOX ESCAPE: read /home/main/.ssh/id_rsa");
    }
    // 3. Attempt outbound network connection
    if TcpStream::connect_timeout(
        &"1.1.1.1:80".parse().unwrap(),
        Duration::from_millis(200),
    )
    .is_ok()
    {
        panic!("SANDBOX ESCAPE: outbound network connection succeeded");
    }
}
"#,
    }
}
