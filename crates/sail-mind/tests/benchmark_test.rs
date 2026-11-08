use async_trait::async_trait;
use sail_mind::{
    BenchmarkRunner, BenchmarkScenario, InferenceProfileManager, backend::InferenceBackend,
    protocol::*,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct MockBenchmarkBackend {
    name: String,
}

#[async_trait]
impl InferenceBackend for MockBenchmarkBackend {
    fn name(&self) -> &str {
        &self.name
    }
    fn aliases(&self) -> Vec<String> {
        vec!["mock".into()]
    }
    async fn start(&self) -> Result<ModelStatus> {
        Ok(self.status().await)
    }
    async fn status(&self) -> ModelStatus {
        ModelStatus {
            version: VERSION,
            state: ModelState::Ready,
            model: self.name.clone(),
            capabilities: None,
            requested_backend: ComputeBackend::Cpu,
            observed_backend: Some(ComputeBackend::Cpu),
            owned_process: false,
            process_id: None,
            last_error: None,
            active_profile: None,
            hardware: None,
            load: None,
        }
    }
    async fn infer(
        &self,
        request: &InferenceRequest,
        events: Option<&mpsc::Sender<InferenceEvent>>,
        _cancel: &CancellationToken,
        _limits: &ResourceLimits,
    ) -> Result<InferenceResponse> {
        if let Some(tx) = events {
            let _ = tx
                .send(InferenceEvent {
                    version: VERSION,
                    request_id: request.request_id,
                    payload: InferenceEventPayload::TextDelta {
                        text: "Benchmark mock delta".into(),
                    },
                })
                .await;
        }

        Ok(InferenceResponse {
            version: VERSION,
            request_id: request.request_id,
            model: self.name.clone(),
            message: Message::text(Role::Assistant, "Benchmark mock response output text"),
            finish_reason: "stop".into(),
            usage: Usage {
                input_tokens: Some(50),
                output_tokens: Some(30),
                reasoning_tokens: None,
            },
            timings: Timings {
                queue_ms: 1,
                first_token_ms: Some(15),
                inference_ms: 60,
            },
        })
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn benchmark_runner_executes_scenarios_and_computes_metrics() {
    let backend = MockBenchmarkBackend {
        name: "test-model".into(),
    };
    let scenario = BenchmarkScenario::voice_turn();
    let record = BenchmarkRunner::run_scenario(&backend, &scenario)
        .await
        .expect("scenario execution");

    assert_eq!(record.scenario, "voice_turn_latency");
    assert!(record.success);
    assert_eq!(record.prompt_tokens, 50);
    assert_eq!(record.output_tokens, 30);
    assert!(record.ttft_ms > 0);
    assert!(record.prompt_eval_tokens_per_sec > 0.0);
    assert!(record.decode_tokens_per_sec > 0.0);
}

#[tokio::test]
async fn benchmark_runner_suite_aggregates_percentiles() {
    let backend = MockBenchmarkBackend {
        name: "test-model".into(),
    };
    let profile = InferenceProfileManager::cpu_tuned("test-model");
    let scenarios = vec![
        BenchmarkScenario::voice_turn(),
        BenchmarkScenario::agent_reasoning(),
        BenchmarkScenario::document_prefill(),
    ];

    let summary = BenchmarkRunner::run_suite(&backend, profile, &scenarios)
        .await
        .expect("suite execution");

    assert_eq!(summary.records.len(), 3);
    assert_eq!(summary.model, "test-model");
    assert!(summary.p50_ttft_ms > 0);
    assert!(summary.p95_ttft_ms >= summary.p50_ttft_ms);
    assert!(summary.avg_decode_tok_per_sec > 0.0);
}
