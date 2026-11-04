//! Credential-scoped OpenAI-compatible text connector. No redirects or agent tools.
use async_trait::async_trait;
use sail_common::types::SubsystemId;
use sail_guardian::{CredentialHandle, CredentialStore};
use sail_mind::external::{ExternalFailure, ExternalReply, ExternalTextTransport};
use sail_protocol::{Generation, Usage};
use std::{sync::Arc, time::Duration};

pub struct OpenAiTextConnector {
    endpoint: String,
    provider: String,
    client: reqwest::Client,
    vault: Arc<CredentialStore>,
    handle: CredentialHandle,
}

impl OpenAiTextConnector {
    /// `endpoint` is the complete HTTPS chat-completions URL. Credentials are scoped to
    /// `external:<provider>:infer`; the provider identity is bound in Guardian's approval.
    pub fn new(
        endpoint: &str,
        provider: &str,
        vault: Arc<CredentialStore>,
        handle: CredentialHandle,
    ) -> Result<Self, &'static str> {
        Self::build(endpoint, provider, vault, handle, false)
    }

    fn build(
        endpoint: &str,
        provider: &str,
        vault: Arc<CredentialStore>,
        handle: CredentialHandle,
        loopback_test: bool,
    ) -> Result<Self, &'static str> {
        let url = url::Url::parse(endpoint).map_err(|_| "invalid external endpoint")?;
        let host = url.host_str().ok_or("external endpoint needs a host")?;
        let loopback = host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
        if !(url.scheme() == "https" || (loopback_test && loopback && url.scheme() == "http"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.path().ends_with("/chat/completions")
        {
            return Err(
                "external endpoints require HTTPS without embedded credentials, queries or fragments",
            );
        }
        if handle.owner != SubsystemId::Bridge
            || handle.scope != format!("external:{provider}:infer")
        {
            return Err("external credential must have an exact provider inference scope");
        }
        vault
            .execute_with_scoped_action(
                &handle,
                SubsystemId::Bridge,
                "external",
                Some(provider),
                "infer",
                |_| (),
            )
            .map_err(|_| "external credential is unavailable or expired")?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|_| "cannot construct external HTTP connector")?;
        Ok(Self {
            endpoint: url.to_string(),
            provider: provider.into(),
            client,
            vault,
            handle,
        })
    }
}

#[async_trait]
impl ExternalTextTransport for OpenAiTextConnector {
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn send(
        &self,
        text: &str,
        model: &str,
        generation: &Generation,
        response_limit: usize,
    ) -> Result<ExternalReply, ExternalFailure> {
        let body = serde_json::json!({"model":model,"messages":[{"role":"user","content":text}],
            "max_tokens":generation.max_tokens,"temperature":generation.temperature,"stream":false});
        let request = self
            .vault
            .execute_with_scoped_action(
                &self.handle,
                SubsystemId::Bridge,
                "external",
                Some(&self.provider),
                "infer",
                |secret| {
                    self.client
                        .post(&self.endpoint)
                        .bearer_auth(secret)
                        .json(&body)
                },
            )
            .map_err(|_| ExternalFailure::Rejected)?;
        let mut response = request
            .send()
            .await
            .map_err(|_| ExternalFailure::Unavailable)?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.parse::<u64>().ok().or_else(|| {
                        chrono::DateTime::parse_from_rfc2822(v).ok().map(|date| {
                            date.timestamp()
                                .saturating_sub(chrono::Utc::now().timestamp())
                                .max(1) as u64
                        })
                    })
                });
            return Err(ExternalFailure::RateLimited {
                retry_after_seconds,
            });
        }
        if !response.status().is_success() {
            return Err(ExternalFailure::Rejected);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ExternalFailure::Unavailable)?
        {
            if bytes.len().saturating_add(chunk.len()) > response_limit {
                return Err(ExternalFailure::Protocol);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ExternalFailure::Protocol)?;
        let message = &value["choices"][0]["message"];
        if message["role"].as_str() != Some("assistant")
            || message
                .get("tool_calls")
                .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
            || message.get("function_call").is_some_and(|v| !v.is_null())
        {
            return Err(ExternalFailure::Protocol);
        }
        let text = message["content"]
            .as_str()
            .ok_or(ExternalFailure::Protocol)?
            .to_string();
        let model = value["model"]
            .as_str()
            .ok_or(ExternalFailure::Protocol)?
            .to_string();
        let token_count = |key: &str| -> Result<Option<u32>, ExternalFailure> {
            match value["usage"].get(key).filter(|v| !v.is_null()) {
                None => Ok(None),
                Some(v) => v
                    .as_u64()
                    .and_then(|n| n.try_into().ok())
                    .map(Some)
                    .ok_or(ExternalFailure::Protocol),
            }
        };
        let finish_reason = value["choices"][0]["finish_reason"]
            .as_str()
            .filter(|reason| matches!(*reason, "stop" | "length"))
            .ok_or(ExternalFailure::Protocol)?
            .to_string();
        Ok(ExternalReply {
            text,
            model,
            usage: Usage {
                input_tokens: token_count("prompt_tokens")?,
                output_tokens: token_count("completion_tokens")?,
            },
            finish_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use sail_common::resources::{AdmissionPolicy, GIB, HostProbe, ResourceScheduler};
    use sail_guardian::{AutonomyPreset, GuardianSubsystem};
    use sail_mind::external::{ExternalBackend, ExternalProviderConfig, ledger::UsageLedger};
    use sail_protocol::{ErrorCode, InferenceRequest, ResourceLimits, external::AttemptOutcome};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio_util::sync::CancellationToken;

    struct IdleHost;
    impl HostProbe for IdleHost {
        fn sample(&self) -> std::io::Result<sail_protocol::resources::HostSample> {
            Ok(sail_protocol::resources::HostSample {
                available_ram_bytes: 16 * GIB,
                total_ram_bytes: 32 * GIB,
                swap_used_bytes: 0,
                swap_total_bytes: GIB,
                logical_cpus: 12,
                memory_pressure_avg10: Some(0.0),
            })
        }
    }
    struct Fixture {
        guardian: GuardianSubsystem,
        backend: Arc<ExternalBackend>,
        ledger: Arc<UsageLedger>,
        calls: Arc<AtomicUsize>,
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
        server: tokio::task::JoinHandle<()>,
        path: std::path::PathBuf,
        scheduler: Arc<ResourceScheduler>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
            let _ = std::fs::remove_file(&self.path);
        }
    }
    async fn fixture(
        status: StatusCode,
        response: serde_json::Value,
        delay: Duration,
        cap: u32,
    ) -> Fixture {
        let guardian = GuardianSubsystem::new();
        // Even Autonomous mode must not bypass the external egress approval.
        guardian
            .policy()
            .set_autonomy_preset(AutonomyPreset::Autonomous);
        let vault = guardian.credentials();
        let handle = vault
            .store_credential(
                SubsystemId::Bridge,
                "test-key",
                "secret-api-key",
                "external:test:infer",
                chrono::Duration::hours(1),
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new().route(
            "/v1/chat/completions",
            post({
                let calls = calls.clone();
                let bodies = bodies.clone();
                move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                    let calls = calls.clone();
                    let bodies = bodies.clone();
                    let response = response.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer secret-api-key");
                        calls.fetch_add(1, Ordering::SeqCst);
                        bodies.lock().unwrap().push(body);
                        tokio::time::sleep(delay).await;
                        (status, [("retry-after", "3600")], Json(response))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let transport = Arc::new(
            OpenAiTextConnector::build(&endpoint, "test", vault.clone(), handle, true).unwrap(),
        );
        let path =
            std::env::temp_dir().join(format!("sail-external-http-{}.db", uuid::Uuid::new_v4()));
        let ledger = Arc::new(UsageLedger::open(&path).unwrap());
        let scheduler = Arc::new(ResourceScheduler::new(
            Arc::new(IdleHost),
            AdmissionPolicy::default(),
        ));
        let backend = Arc::new(
            ExternalBackend::new(
                ExternalProviderConfig {
                    provider: "test".into(),
                    model: "test-model".into(),
                    daily_cap: cap,
                    entity_names: vec!["Alice".into()],
                },
                transport,
                ledger.clone(),
                guardian.broker(),
                vault,
                scheduler.clone(),
            )
            .unwrap(),
        );
        Fixture {
            guardian,
            backend,
            ledger,
            calls,
            bodies,
            server,
            path,
            scheduler,
        }
    }
    fn answer() -> serde_json::Value {
        serde_json::json!({"model":"test-model-v1","choices":[{"message":{"role":"assistant","content":"Ownership prevents aliasing bugs. secret-api-key"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":6}})
    }
    fn approve(f: &Fixture, request: &InferenceRequest) -> uuid::Uuid {
        let capability = f
            .backend
            .approval_capability(request, &ResourceLimits::default())
            .unwrap();
        let session = f.guardian.approval().create_session_for(
            Some(request.request_id),
            SubsystemId::Mind,
            capability,
            "owner test approval",
        );
        f.guardian
            .approval()
            .resolve_session(session.session_id, true)
            .unwrap()
            .token_id
    }

    #[tokio::test]
    async fn external_provider_cannot_be_the_implicit_mind_backend() {
        let f = fixture(StatusCode::OK, answer(), Duration::ZERO, 2).await;
        assert!(
            sail_mind::MindService::with_backends(
                vec![f.backend.clone()],
                ResourceLimits::default()
            )
            .is_err()
        );
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn authorized_text_crosses_real_http_boundary_once_with_metadata_only_receipt() {
        let f = fixture(StatusCode::OK, answer(), Duration::ZERO, 2).await;
        let request = InferenceRequest::text(
            "Explain ownership for Alice at alice@example.org; secret-api-key",
        );
        let limits = ResourceLimits::default();
        let cancel = CancellationToken::new();
        assert_eq!(
            f.backend
                .infer_authorized(&request, None, &cancel, &limits)
                .await
                .unwrap_err()
                .code,
            ErrorCode::ForbiddenEndpoint
        );
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
        assert!(f.ledger.receipt(request.request_id).unwrap().is_none());
        let token = approve(&f, &request);
        let response = f
            .backend
            .infer_authorized(&request, Some(token), &cancel, &limits)
            .await
            .unwrap();
        assert_eq!(response.model, "test:test-model-v1");
        assert!(
            !response
                .message
                .content
                .text()
                .unwrap()
                .contains("secret-api-key")
        );
        assert_eq!(response.usage.output_tokens, Some(6));
        let body = f.bodies.lock().unwrap()[0].to_string();
        for private in ["Alice", "alice@example", "secret-api-key"] {
            assert!(!body.contains(private));
        }
        assert!(!body.contains("tools"));
        let receipt = f.ledger.receipt(request.request_id).unwrap().unwrap();
        assert_eq!(receipt.outcome, AttemptOutcome::Completed);
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains("ownership")
        );
        assert!(
            f.backend
                .infer_authorized(&request, Some(token), &cancel, &limits)
                .await
                .is_err()
        );
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn approval_cannot_authorize_changed_generation_or_request_identity() {
        let f = fixture(StatusCode::OK, answer(), Duration::ZERO, 2).await;
        let mut request = InferenceRequest::text("Explain ownership");
        let token = approve(&f, &request);
        request.generation.max_tokens += 1;
        assert!(
            f.backend
                .infer_authorized(
                    &request,
                    Some(token),
                    &CancellationToken::new(),
                    &ResourceLimits::default()
                )
                .await
                .is_err()
        );
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rate_limit_and_daily_cap_block_transport() {
        for (status, cap) in [(StatusCode::TOO_MANY_REQUESTS, 10), (StatusCode::OK, 1)] {
            let f = fixture(status, answer(), Duration::ZERO, cap).await;
            for iteration in 0..2 {
                let request = InferenceRequest::text("Explain ownership");
                let result = f
                    .backend
                    .infer_authorized(
                        &request,
                        Some(approve(&f, &request)),
                        &CancellationToken::new(),
                        &ResourceLimits::default(),
                    )
                    .await;
                if iteration == 1 {
                    assert_eq!(result.unwrap_err().code, ErrorCode::ResourceLimit);
                }
            }
            assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn voice_preemption_retains_uncertain_receipt_and_never_replays() {
        let f = fixture(StatusCode::OK, answer(), Duration::from_secs(30), 10).await;
        let request = InferenceRequest::text("Explain ownership");
        let token = approve(&f, &request);
        let cancel = CancellationToken::new();
        let limits = ResourceLimits::default();
        let run = f
            .backend
            .infer_authorized(&request, Some(token), &cancel, &limits);
        tokio::pin!(run);
        tokio::select! {
            _ = &mut run => panic!("request completed before voice"),
            _ = async { while f.calls.load(Ordering::SeqCst) == 0 { tokio::task::yield_now().await; } } => {}
        }
        let _voice = f.scheduler.voice_turn();
        assert_eq!(run.await.unwrap_err().code, ErrorCode::Busy);
        assert!(
            !cancel.is_cancelled(),
            "voice must not cancel the parent task"
        );
        assert_eq!(
            f.ledger
                .receipt(request.request_id)
                .unwrap()
                .unwrap()
                .outcome,
            AttemptOutcome::Uncertain
        );
        let retry = approve(&f, &request);
        drop(_voice);
        assert!(
            f.backend
                .infer_authorized(&request, Some(retry), &CancellationToken::new(), &limits)
                .await
                .is_err()
        );
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_tool_calls_and_oversized_replies_are_never_accepted() {
        for response in [
            serde_json::json!({"model":"test-model","choices":[{"message":{"role":"assistant","content":"done","tool_calls":[{"function":{"name":"shell"}}]},"finish_reason":"tool_calls"}]}),
            serde_json::json!({"oversized":"x".repeat(200_000)}),
        ] {
            let f = fixture(StatusCode::OK, response, Duration::ZERO, 10).await;
            let request = InferenceRequest::text("Explain ownership");
            let result = f
                .backend
                .infer_authorized(
                    &request,
                    Some(approve(&f, &request)),
                    &CancellationToken::new(),
                    &ResourceLimits::default(),
                )
                .await;
            assert_eq!(result.unwrap_err().code, ErrorCode::BackendProtocol);
            assert_eq!(
                f.ledger
                    .receipt(request.request_id)
                    .unwrap()
                    .unwrap()
                    .outcome,
                AttemptOutcome::Uncertain
            );
        }
    }

    #[test]
    fn connector_rejects_credential_leaks_and_unsafe_endpoints() {
        let vault = Arc::new(CredentialStore::new());
        let handle = vault
            .store_credential(
                SubsystemId::Bridge,
                "key",
                "secret",
                "external:test:infer",
                chrono::Duration::hours(1),
            )
            .unwrap();
        for endpoint in [
            "http://example.org/v1/chat/completions",
            "http://127.0.0.1/v1/chat/completions",
            "https://user@example.org/v1/chat/completions",
            "https://user:pass@example.org/v1/chat/completions",
            "https://example.org/v1/chat/completions?key=secret",
            "https://example.org/v1/chat/completions#secret",
            "https://example.org/wrong-path",
        ] {
            assert!(
                OpenAiTextConnector::new(endpoint, "test", vault.clone(), handle.clone()).is_err()
            );
        }
        let connector = OpenAiTextConnector::new(
            "https://example.org/v1/chat/completions",
            "test",
            vault.clone(),
            handle.clone(),
        )
        .unwrap();
        assert_eq!(
            connector.endpoint(),
            "https://example.org/v1/chat/completions"
        );
        assert!(
            OpenAiTextConnector::build(
                "http://203.0.113.1/v1/chat/completions",
                "test",
                vault.clone(),
                handle.clone(),
                true
            )
            .is_err()
        );
        assert!(
            OpenAiTextConnector::new(
                "https://example.org/v1/chat/completions",
                "other",
                vault,
                handle
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn text_protocol_accepts_empty_optional_tool_fields_but_rejects_actual_tools_and_wrong_roles()
     {
        for (message, accepted) in [
            (
                serde_json::json!({"role":"assistant","content":"text","tool_calls":null,"function_call":null}),
                true,
            ),
            (
                serde_json::json!({"role":"assistant","content":"text","tool_calls":[]}),
                true,
            ),
            (serde_json::json!({"role":"user","content":"text"}), false),
            (
                serde_json::json!({"role":"assistant","content":"text","function_call":{"name":"shell"}}),
                false,
            ),
            (
                serde_json::json!({"role":"assistant","content":"text","tool_calls":{}}),
                false,
            ),
            (
                serde_json::json!({"role":"assistant","content":"text","tool_calls":[{}]}),
                false,
            ),
        ] {
            let response = serde_json::json!({"model":"test-model","choices":[{"message":message,"finish_reason":"stop"}]});
            let f = fixture(StatusCode::OK, response, Duration::ZERO, 10).await;
            let request = InferenceRequest::text("explain ownership");
            let result = f
                .backend
                .infer_authorized(
                    &request,
                    Some(approve(&f, &request)),
                    &CancellationToken::new(),
                    &ResourceLimits::default(),
                )
                .await;
            assert_eq!(
                result.is_ok(),
                accepted,
                "unexpected acceptance for {message}"
            );
            if !accepted {
                assert_eq!(result.unwrap_err().code, ErrorCode::BackendProtocol);
            }
        }
    }

    #[tokio::test]
    async fn http_response_byte_budget_accepts_exact_size_and_rejects_larger_bodies() {
        let response = answer();
        let bytes = serde_json::to_vec(&response).unwrap().len();
        for (limit, accepted) in [
            (bytes, true),
            (bytes + 1, true),
            (bytes - 1, false),
            (bytes - 2, false),
        ] {
            let f = fixture(StatusCode::OK, response.clone(), Duration::ZERO, 10).await;
            let request = InferenceRequest::text("explain ownership");
            let limits = ResourceLimits {
                max_response_bytes: limit,
                ..Default::default()
            };
            let result = f
                .backend
                .infer_authorized(
                    &request,
                    Some(approve(&f, &request)),
                    &CancellationToken::new(),
                    &limits,
                )
                .await;
            assert_eq!(
                result.is_ok(),
                accepted,
                "HTTP response length {bytes}, limit {limit}"
            );
            if !accepted {
                assert_eq!(result.unwrap_err().code, ErrorCode::BackendProtocol);
            }
        }
    }
}
