//! Validated solve-rate tasks for `eval`.
//!
//! Every case is dependency-free (builds with `cargo test --offline`), fails before it is
//! fixed, and passes with its reference solution. `eval::tests` enforces both properties,
//! so a case can never be "solved" by doing nothing.

/// A task definition: initial files, a hidden test appended to `hidden_test.0`, and the
/// reference solution (full replacement contents for the files it changes).
pub struct CaseSpec {
    pub id: u64,
    pub category: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub files: &'static [(&'static str, &'static str)],
    pub hidden_test: Option<(&'static str, &'static str)>,
    pub reference: &'static [(&'static str, &'static str)],
}

pub const CASES: &[CaseSpec] = &[
    CaseSpec {
        id: 201,
        category: "compile_fix",
        title: "Fix E0502 borrow error in Cache::evict_oldest",
        description: "src/cache.rs fails to compile with E0502 (cannot borrow `self.items` as mutable because it is also borrowed as immutable) in `Cache::evict_oldest`. Fix it without changing behavior: evict and return the entry with the smallest value, or None when empty.",
        files: &[(
            "src/cache.rs",
            r#"pub struct Cache {
    pub items: Vec<(String, u64)>,
}

impl Cache {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push(&mut self, key: String, val: u64) {
        self.items.push((key, val));
    }

    pub fn evict_oldest(&mut self) -> Option<(String, u64)> {
        let oldest = self.items.iter().min_by_key(|(_, val)| *val)?;
        self.items.retain(|(k, _)| k != &oldest.0);
        Some(oldest.clone())
    }
}
"#,
        )],
        hidden_test: Some((
            "src/cache.rs",
            r#"#[cfg(test)]
mod hidden_eval_tests {
    use super::*;

    #[test]
    fn evicts_smallest_value() {
        let mut c = Cache::new();
        c.push("a".into(), 10);
        c.push("b".into(), 5);
        c.push("c".into(), 20);
        assert_eq!(c.evict_oldest(), Some(("b".into(), 5)));
        assert_eq!(c.items.len(), 2);
        assert_eq!(Cache::new().evict_oldest(), None);
    }
}
"#,
        )),
        reference: &[(
            "src/cache.rs",
            r#"pub struct Cache {
    pub items: Vec<(String, u64)>,
}

impl Cache {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push(&mut self, key: String, val: u64) {
        self.items.push((key, val));
    }

    pub fn evict_oldest(&mut self) -> Option<(String, u64)> {
        let idx = self
            .items
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, val))| *val)?
            .0;
        Some(self.items.remove(idx))
    }
}
"#,
        )],
    },
    CaseSpec {
        id: 202,
        category: "bugfix",
        title: "Fix out-of-bounds panic in chunk_tokens for a partial final chunk",
        description: "`chunk_tokens` in src/chunker.rs panics with an out-of-range slice when `tokens.len()` is not a multiple of `chunk_size`. The final chunk must contain the remaining tokens. Empty input returns an empty Vec.",
        files: &[(
            "src/chunker.rs",
            r#"/// Split tokens into consecutive chunks of at most `chunk_size` tokens.
pub fn chunk_tokens(tokens: &[u32], chunk_size: usize) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < tokens.len() {
        let end = start + chunk_size;
        out.push(tokens[start..end].to_vec());
        start = end;
    }
    out
}
"#,
        )],
        hidden_test: Some((
            "src/chunker.rs",
            r#"#[cfg(test)]
mod hidden_eval_tests {
    use super::*;

    #[test]
    fn partial_final_chunk() {
        assert_eq!(
            chunk_tokens(&[1, 2, 3, 4, 5], 2),
            vec![vec![1, 2], vec![3, 4], vec![5]]
        );
        assert_eq!(chunk_tokens(&[1, 2, 3, 4], 2), vec![vec![1, 2], vec![3, 4]]);
        assert!(chunk_tokens(&[], 3).is_empty());
    }
}
"#,
        )),
        reference: &[(
            "src/chunker.rs",
            r#"/// Split tokens into consecutive chunks of at most `chunk_size` tokens.
pub fn chunk_tokens(tokens: &[u32], chunk_size: usize) -> Vec<Vec<u32>> {
    tokens.chunks(chunk_size).map(|c| c.to_vec()).collect()
}
"#,
        )],
    },
    CaseSpec {
        id: 203,
        category: "feature",
        title: "Add optional priority and trace_id to InferenceRequest",
        description: "Extend `InferenceRequest` in src/protocol.rs with `pub priority: Option<u8>` and `pub trace_id: Option<String>`. `InferenceRequest::new` must default both to None. Add builder methods `with_priority(self, p: u8) -> Self` and `with_trace_id(self, id: impl Into<String>) -> Self`.",
        files: &[(
            "src/protocol.rs",
            r#"#[derive(Debug, Clone, PartialEq)]
pub struct InferenceRequest {
    pub prompt: String,
    pub max_tokens: u32,
}

impl InferenceRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            max_tokens: 256,
        }
    }
}
"#,
        )],
        hidden_test: Some((
            "src/protocol.rs",
            r#"#[cfg(test)]
mod hidden_eval_tests {
    use super::*;

    #[test]
    fn optional_fields_default_and_build() {
        let r = InferenceRequest::new("hi");
        assert_eq!(r.priority, None);
        assert_eq!(r.trace_id, None);
        let r = r.with_priority(3).with_trace_id("t-1");
        assert_eq!(r.priority, Some(3));
        assert_eq!(r.trace_id.as_deref(), Some("t-1"));
        assert_eq!(r.max_tokens, 256);
    }
}
"#,
        )),
        reference: &[(
            "src/protocol.rs",
            r#"#[derive(Debug, Clone, PartialEq)]
pub struct InferenceRequest {
    pub prompt: String,
    pub max_tokens: u32,
    pub priority: Option<u8>,
    pub trace_id: Option<String>,
}

impl InferenceRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            max_tokens: 256,
            priority: None,
            trace_id: None,
        }
    }

    pub fn with_priority(mut self, p: u8) -> Self {
        self.priority = Some(p);
        self
    }

    pub fn with_trace_id(mut self, id: impl Into<String>) -> Self {
        self.trace_id = Some(id.into());
        self
    }
}
"#,
        )],
    },
    CaseSpec {
        id: 204,
        category: "feature",
        title: "Add --dry-run and --format flags to the runner CLI parser",
        description: "In src/cli.rs, extend `CliOptions` with `pub dry_run: bool` (set by `--dry-run`) and `pub format: OutputFormat` (set by `--format text` or `--format json`, default `OutputFormat::Text`). Add `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum OutputFormat { Text, Json }`. An unknown format value or a missing value after `--format` must return `Err`. Existing `--verbose` behavior is unchanged.",
        files: &[(
            "src/cli.rs",
            r#"#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    pub verbose: bool,
}

pub fn parse(args: &[&str]) -> Result<CliOptions, String> {
    let mut opts = CliOptions { verbose: false };
    for arg in args {
        match *arg {
            "--verbose" | "-v" => opts.verbose = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(opts)
}
"#,
        )],
        hidden_test: Some((
            "src/cli.rs",
            r#"#[cfg(test)]
mod hidden_eval_tests {
    use super::*;

    #[test]
    fn dry_run_and_format() {
        let o = parse(&["--dry-run", "--format", "json", "-v"]).unwrap();
        assert!(o.dry_run && o.verbose);
        assert_eq!(o.format, OutputFormat::Json);
        let d = parse(&[]).unwrap();
        assert!(!d.dry_run);
        assert_eq!(d.format, OutputFormat::Text);
        assert!(parse(&["--format", "xml"]).is_err());
        assert!(parse(&["--format"]).is_err());
    }
}
"#,
        )),
        reference: &[(
            "src/cli.rs",
            r#"#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    pub verbose: bool,
    pub dry_run: bool,
    pub format: OutputFormat,
}

pub fn parse(args: &[&str]) -> Result<CliOptions, String> {
    let mut opts = CliOptions {
        verbose: false,
        dry_run: false,
        format: OutputFormat::Text,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "--verbose" | "-v" => opts.verbose = true,
            "--dry-run" => opts.dry_run = true,
            "--format" => {
                opts.format = match it.next().copied() {
                    Some("text") => OutputFormat::Text,
                    Some("json") => OutputFormat::Json,
                    Some(other) => return Err(format!("unknown format: {other}")),
                    None => return Err("--format requires a value".into()),
                }
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(opts)
}
"#,
        )],
    },
    CaseSpec {
        id: 205,
        category: "error_handling",
        title: "Return Result instead of panicking on unsupported sample rates",
        description: "`resample_pcm` in src/resample.rs calls `unwrap()` and panics for unsupported rates. Change it to return `Result<Vec<f32>, ResampleError>` where `#[derive(Debug, PartialEq, Eq)] pub enum ResampleError { UnsupportedRate(u32) }` carries the first unsupported rate (checking `from_rate` before `to_rate`). Supported rates stay 16000, 24000 and 48000, and the resampling math is unchanged.",
        files: &[(
            "src/resample.rs",
            r#"const SUPPORTED: [u32; 3] = [16_000, 24_000, 48_000];

fn rate_index(rate: u32) -> Option<usize> {
    SUPPORTED.iter().position(|r| *r == rate)
}

/// Nearest-sample resampling between supported rates.
pub fn resample_pcm(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    rate_index(from_rate).unwrap();
    rate_index(to_rate).unwrap();
    let out_len = input.len() * to_rate as usize / from_rate as usize;
    (0..out_len)
        .map(|i| input[i * from_rate as usize / to_rate as usize])
        .collect()
}
"#,
        )],
        hidden_test: Some((
            "src/resample.rs",
            r#"#[cfg(test)]
mod hidden_eval_tests {
    use super::*;

    #[test]
    fn unsupported_rates_are_errors() {
        assert_eq!(
            resample_pcm(&[0.0; 4], 11_025, 16_000),
            Err(ResampleError::UnsupportedRate(11_025))
        );
        assert_eq!(
            resample_pcm(&[0.0; 4], 16_000, 44_100),
            Err(ResampleError::UnsupportedRate(44_100))
        );
        let down = resample_pcm(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 48_000, 16_000).unwrap();
        assert_eq!(down, vec![1.0, 4.0]);
    }
}
"#,
        )),
        reference: &[(
            "src/resample.rs",
            r#"const SUPPORTED: [u32; 3] = [16_000, 24_000, 48_000];

#[derive(Debug, PartialEq, Eq)]
pub enum ResampleError {
    UnsupportedRate(u32),
}

fn rate_index(rate: u32) -> Option<usize> {
    SUPPORTED.iter().position(|r| *r == rate)
}

/// Nearest-sample resampling between supported rates.
pub fn resample_pcm(input: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>, ResampleError> {
    rate_index(from_rate).ok_or(ResampleError::UnsupportedRate(from_rate))?;
    rate_index(to_rate).ok_or(ResampleError::UnsupportedRate(to_rate))?;
    let out_len = input.len() * to_rate as usize / from_rate as usize;
    Ok((0..out_len)
        .map(|i| input[i * from_rate as usize / to_rate as usize])
        .collect())
}
"#,
        )],
    },
    CaseSpec {
        id: 206,
        category: "bugfix",
        title: "Make truncate_prompt count characters and respect UTF-8 boundaries",
        description: "`truncate_prompt(s, max_chars)` in src/sanitizer.rs slices by bytes, which panics inside multi-byte characters and counts bytes instead of characters. It must return at most `max_chars` Unicode scalar values, unchanged when the input is shorter.",
        files: &[(
            "src/sanitizer.rs",
            r#"/// Truncate a prompt to at most `max_chars` characters.
pub fn truncate_prompt(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }
    s[..max_chars].to_string()
}
"#,
        )],
        hidden_test: Some((
            "src/sanitizer.rs",
            r#"#[cfg(test)]
mod hidden_eval_tests {
    use super::*;

    #[test]
    fn counts_characters_not_bytes() {
        assert_eq!(truncate_prompt("héllo wörld", 7), "héllo w");
        assert_eq!(truncate_prompt("ab🚀cd", 3), "ab🚀");
        assert_eq!(truncate_prompt("short", 10), "short");
        assert_eq!(truncate_prompt("", 0), "");
    }
}
"#,
        )),
        reference: &[(
            "src/sanitizer.rs",
            r#"/// Truncate a prompt to at most `max_chars` characters.
pub fn truncate_prompt(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}
"#,
        )],
    },
    CaseSpec {
        id: 207,
        category: "bugfix",
        title: "Make EventEnvelope::matches_topic segment-aware",
        description: "`EventEnvelope::matches_topic(prefix)` in src/envelope.rs uses `starts_with`, so prefix `audio` wrongly matches topic `audiobook.play`. A topic matches when it equals the prefix or starts with the prefix followed by `.`. An empty prefix matches every topic.",
        files: &[(
            "src/envelope.rs",
            r#"pub struct EventEnvelope {
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
"#,
        )],
        hidden_test: Some((
            "src/envelope.rs",
            r#"#[cfg(test)]
mod hidden_eval_tests {
    use super::*;

    #[test]
    fn segment_boundaries() {
        let e = EventEnvelope::new("audio.capture.start", "");
        assert!(e.matches_topic("audio"));
        assert!(e.matches_topic("audio.capture"));
        assert!(e.matches_topic("audio.capture.start"));
        assert!(e.matches_topic(""));
        assert!(!e.matches_topic("audio.cap"));
        assert!(!EventEnvelope::new("audiobook.play", "").matches_topic("audio"));
    }
}
"#,
        )),
        reference: &[(
            "src/envelope.rs",
            r#"pub struct EventEnvelope {
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
        prefix.is_empty()
            || self.topic == prefix
            || self
                .topic
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
    }
}
"#,
        )],
    },
];
