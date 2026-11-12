//! Provider adapter against a local fake OpenAI-compatible server. No network.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use sail_mind::escalation::ProviderConfig;
use sail_mind::escalation::provider::{ExternalProvider, OpenAiCompatible, ProviderError};

#[derive(Clone, Default)]
struct Seen {
    authorization: Option<String>,
    body: Value,
}

fn config(base: &str, key_env: &str) -> ProviderConfig {
    ProviderConfig {
        id: "fake-openai".to_string(),
        adapter: "openai_compatible".to_string(),
        base_url: format!("{base}/v1"),
        model: "client-model".to_string(),
        api_key_env: key_env.to_string(),
        kinds: Vec::new(),
        input_price_micros_per_mtok: 0,
        output_price_micros_per_mtok: 0,
        enabled: true,
    }
}

async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local address");
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("fake provider");
    });
    (format!("http://127.0.0.1:{}", address.port()), task)
}

fn set_key(name: &str, value: &str) {
    // The test process owns these uniquely named variables.
    unsafe { std::env::set_var(name, value) };
}

#[tokio::test]
async fn success_sends_the_openai_shape_and_parses_the_reply() {
    set_key("SAIL_TEST_LLM_KEY_OK", "test-key");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let state = seen.clone();
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |State(state): State<Arc<Mutex<Seen>>>,
                      headers: HeaderMap,
                      Json(body): Json<Value>| async move {
                    {
                        let mut seen = state.lock().expect("seen");
                        seen.authorization = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned);
                        seen.body = body;
                    }
                    Json(json!({
                        "model": "server-model-v2",
                        "choices": [{"message": {"role": "assistant", "content": "Use a queue."}}],
                        "usage": {"prompt_tokens": 11, "completion_tokens": 7},
                    }))
                },
            ),
        )
        .with_state(state);
    let (base, server) = serve(app).await;

    let provider = OpenAiCompatible::new(config(&base, "SAIL_TEST_LLM_KEY_OK")).expect("provider");
    let reply = provider
        .complete(
            "system instructions",
            "the redacted question",
            256,
            CancellationToken::new(),
        )
        .await
        .expect("reply");
    assert_eq!(reply.text, "Use a queue.");
    assert_eq!(reply.model, "server-model-v2");
    assert_eq!(reply.input_tokens, 11);
    assert_eq!(reply.output_tokens, 7);

    let seen = seen.lock().expect("seen").clone();
    assert_eq!(seen.authorization.as_deref(), Some("Bearer test-key"));
    assert_eq!(seen.body["model"], "client-model");
    assert_eq!(seen.body["messages"][0]["role"], "system");
    assert_eq!(seen.body["messages"][0]["content"], "system instructions");
    assert_eq!(seen.body["messages"][1]["role"], "user");
    assert_eq!(seen.body["messages"][1]["content"], "the redacted question");
    assert_eq!(seen.body["max_tokens"], 256);
    server.abort();
}

#[tokio::test]
async fn reply_without_a_model_falls_back_to_the_configured_one() {
    set_key("SAIL_TEST_LLM_KEY_NO_MODEL", "test-key");
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Json(json!({
                "choices": [{"message": {"content": "answer"}}],
            }))
        }),
    );
    let (base, server) = serve(app).await;
    let provider =
        OpenAiCompatible::new(config(&base, "SAIL_TEST_LLM_KEY_NO_MODEL")).expect("provider");
    let reply = provider
        .complete("system", "question", 32, CancellationToken::new())
        .await
        .expect("reply");
    assert_eq!(reply.model, "client-model");
    assert_eq!(reply.input_tokens, 0);
    server.abort();
}

#[tokio::test]
async fn rate_limit_reply_carries_retry_after() {
    set_key("SAIL_TEST_LLM_KEY_429", "test-key");
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "42")],
                Json(json!({"error": "slow down"})),
            )
        }),
    );
    let (base, server) = serve(app).await;
    let provider = OpenAiCompatible::new(config(&base, "SAIL_TEST_LLM_KEY_429")).expect("provider");
    let error = provider
        .complete("system", "question", 32, CancellationToken::new())
        .await
        .expect_err("must be rate limited");
    match error {
        ProviderError::RateLimited { retry_after } => {
            let retry_after = retry_after.expect("Retry-After parsed");
            let wait = (retry_after - chrono::Utc::now()).num_seconds();
            assert!((35..=45).contains(&wait), "unexpected wait {wait}s");
        }
        other => panic!("unexpected error {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn http_errors_are_not_answers() {
    set_key("SAIL_TEST_LLM_KEY_500", "test-key");
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Body::from("upstream exploded"),
            )
                .into_response()
        }),
    );
    let (base, server) = serve(app).await;
    let provider = OpenAiCompatible::new(config(&base, "SAIL_TEST_LLM_KEY_500")).expect("provider");
    let error = provider
        .complete("system", "question", 32, CancellationToken::new())
        .await
        .expect_err("must fail");
    match error {
        ProviderError::Http(message) => {
            assert!(message.contains("500"), "{message}");
            assert!(message.contains("upstream exploded"), "{message}");
        }
        other => panic!("unexpected error {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn a_body_naming_a_rate_limit_is_rate_limited() {
    set_key("SAIL_TEST_LLM_KEY_BODY_LIMIT", "test-key");
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (StatusCode::BAD_REQUEST, "Too Many Requests for this key").into_response()
        }),
    );
    let (base, server) = serve(app).await;
    let provider =
        OpenAiCompatible::new(config(&base, "SAIL_TEST_LLM_KEY_BODY_LIMIT")).expect("provider");
    let error = provider
        .complete("system", "question", 32, CancellationToken::new())
        .await
        .expect_err("must be rate limited");
    assert!(
        matches!(error, ProviderError::RateLimited { .. }),
        "{error:?}"
    );
    server.abort();
}

#[tokio::test]
async fn cancelling_the_token_stops_the_call() {
    set_key("SAIL_TEST_LLM_KEY_CANCEL", "test-key");
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Json(json!({"choices": [{"message": {"content": "too late"}}]}))
        }),
    );
    let (base, server) = serve(app).await;
    let provider =
        OpenAiCompatible::new(config(&base, "SAIL_TEST_LLM_KEY_CANCEL")).expect("provider");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    let error = provider
        .complete("system", "question", 32, cancel)
        .await
        .expect_err("must be cancelled");
    assert_eq!(error, ProviderError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
    server.abort();
}

#[tokio::test]
async fn unavailable_without_a_key_and_unknown_adapters() {
    let missing = "SAIL_TEST_LLM_KEY_ABSENT_0123456789";
    let provider = OpenAiCompatible::new(config("http://127.0.0.1:9", missing)).expect("provider");
    let error = provider
        .complete("system", "question", 32, CancellationToken::new())
        .await
        .expect_err("must be unavailable");
    assert!(matches!(error, ProviderError::Unavailable(_)), "{error:?}");

    let mut empty = config("http://127.0.0.1:9", "SAIL_TEST_ANY");
    empty.api_key_env = "".into();
    assert!(matches!(
        OpenAiCompatible::new(empty),
        Err(ProviderError::Unavailable(_))
    ));

    let unknown = sail_mind::escalation::provider::build(&ProviderConfig {
        adapter: "codex-cli".into(),
        ..config("https://example.test", "SAIL_TEST_ANY")
    });
    assert!(matches!(unknown, Err(ProviderError::Unavailable(_))));
}

#[test]
fn base_urls_must_be_https_unless_loopback() {
    assert!(OpenAiCompatible::new(config("http://example.com", "SAIL_TEST_ANY")).is_err());
    assert!(OpenAiCompatible::new(config("http://127.0.0.1:8080", "SAIL_TEST_ANY")).is_ok());
    assert!(OpenAiCompatible::new(config("http://localhost:8080", "SAIL_TEST_ANY")).is_ok());
    assert!(OpenAiCompatible::new(config("https://example.com", "SAIL_TEST_ANY")).is_ok());
    let mut bad = config("https://example.com", "SAIL_TEST_ANY");
    bad.base_url = "not a url".into();
    assert!(OpenAiCompatible::new(bad).is_err());
}
