use crate::backend::InferenceBackend;
use sail_protocol::{
    BenchmarkRecord, BenchmarkSummary, InferenceProfile, InferenceRequest, Result,
};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct BenchmarkScenario {
    pub name: String,
    pub prompt: String,
    pub max_tokens: u32,
}

impl BenchmarkScenario {
    pub fn voice_turn() -> Self {
        Self {
            name: "voice_turn_latency".into(),
            prompt: "Good morning SAIL. What background tasks and models are currently active?"
                .into(),
            max_tokens: 48,
        }
    }

    pub fn agent_reasoning() -> Self {
        Self {
            name: "agent_reasoning".into(),
            prompt: "Review the following Rust function and identify any concurrency or allocation bottlenecks:\n\
                ```rust\n\
                pub fn process_items(items: Vec<String>) -> Vec<String> {\n\
                    let mut result = Vec::new();\n\
                    for item in items {\n\
                        if item.starts_with(\"valid\") {\n\
                            result.push(item.to_uppercase());\n\
                        }\n\
                    }\n\
                    result\n\
                }\n\
                ```\n\
                Provide a concise refactoring and explain why it is optimal."
                .into(),
            max_tokens: 128,
        }
    }

    pub fn document_prefill() -> Self {
        let repeated = "The quick brown fox jumps over the lazy dog. Local LLM inference requires careful memory management and thread pinning. ".repeat(25);
        Self {
            name: "document_prefill".into(),
            prompt: format!(
                "Summarize the key invariants from the following text:\n{}",
                repeated
            ),
            max_tokens: 64,
        }
    }
}

pub struct BenchmarkRunner;

impl BenchmarkRunner {
    /// Read peak Resident Set Size (VmHWM or VmRSS) in MB for the current process.
    pub fn read_rss_mb() -> u64 {
        if let Ok(content) = std::fs::read_to_string("/proc/self/status") {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("VmHWM:")
                    && let Some(kb) = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                {
                    return kb / 1024;
                }
            }
        }
        0
    }

    /// Run a single benchmark scenario against an active inference backend.
    pub async fn run_scenario(
        backend: &dyn InferenceBackend,
        scenario: &BenchmarkScenario,
    ) -> Result<BenchmarkRecord> {
        let mut request = InferenceRequest::text(&scenario.prompt);
        request.generation.max_tokens = scenario.max_tokens;
        request.generation.temperature = 0.0;

        let start = Instant::now();
        let cancel = CancellationToken::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);

        // Spawn inference with event streaming
        let limits = sail_protocol::ResourceLimits::default();
        let backend_fut = backend.infer(&request, Some(&tx), &cancel, &limits);
        let mut first_token_time = None;

        tokio::pin!(backend_fut);

        let response = loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    if first_token_time.is_none()
                        && let sail_protocol::InferenceEventPayload::TextDelta { .. } = event.payload
                    {
                        first_token_time = Some(Instant::now());
                    }
                }
                res = &mut backend_fut => {
                    break res?;
                }
            }
        };

        let elapsed = start.elapsed();
        let ttft_ms = first_token_time
            .map(|t| t.duration_since(start).as_millis() as u64)
            .or(response.timings.first_token_ms)
            .unwrap_or(elapsed.as_millis() as u64);

        let prompt_tokens = response
            .usage
            .input_tokens
            .unwrap_or((scenario.prompt.len() / 4) as u32) as usize;
        let output_tokens = response.usage.output_tokens.unwrap_or_else(|| {
            (response
                .message
                .content
                .text()
                .map(|s| s.len())
                .unwrap_or(0)
                / 4) as u32
        }) as usize;

        let prompt_eval_sec = (ttft_ms as f32 / 1000.0).max(0.001);
        let prompt_eval_tokens_per_sec = prompt_tokens as f32 / prompt_eval_sec;

        let decode_sec = (elapsed.as_secs_f32() - (ttft_ms as f32 / 1000.0)).max(0.001);
        let decode_tokens_per_sec = output_tokens as f32 / decode_sec;

        let peak_rss_mb = Self::read_rss_mb();

        Ok(BenchmarkRecord {
            scenario: scenario.name.clone(),
            prompt_tokens,
            output_tokens,
            ttft_ms,
            prompt_eval_tokens_per_sec,
            decode_tokens_per_sec,
            peak_rss_mb,
            success: true,
        })
    }

    /// Execute a standard suite of scenarios and produce a summary.
    pub async fn run_suite(
        backend: &dyn InferenceBackend,
        profile: InferenceProfile,
        scenarios: &[BenchmarkScenario],
    ) -> Result<BenchmarkSummary> {
        let mut records = Vec::new();
        for s in scenarios {
            let rec = Self::run_scenario(backend, s).await?;
            records.push(rec);
        }

        let mut ttfts: Vec<u64> = records.iter().map(|r| r.ttft_ms).collect();
        ttfts.sort_unstable();

        let p50_ttft_ms = if ttfts.is_empty() {
            0
        } else {
            ttfts[ttfts.len() / 2]
        };

        let p95_ttft_ms = if ttfts.is_empty() {
            0
        } else {
            let idx = ((ttfts.len() as f32 * 0.95).ceil() as usize).min(ttfts.len()) - 1;
            ttfts[idx]
        };

        let avg_decode_tok_per_sec = if records.is_empty() {
            0.0
        } else {
            records.iter().map(|r| r.decode_tokens_per_sec).sum::<f32>() / records.len() as f32
        };

        Ok(BenchmarkSummary {
            model: backend.name().to_string(),
            profile,
            records,
            p50_ttft_ms,
            p95_ttft_ms,
            avg_decode_tok_per_sec,
        })
    }
}
