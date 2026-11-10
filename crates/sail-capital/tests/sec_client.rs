//! Client tests against an in-process axum mock server on `127.0.0.1:0`. No request ever
//! reaches sec.gov; the fixtures are synthetic and the only server is the test's own.
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::get;
use sail_capital::evidence::{EdgarApi, MAX_SEC_RESPONSE_BYTES, SourceError};
use sail_capital::sec::EdgarClient;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TICKER_JSON: &str =
    r#"{"0":{"cik_str":1234567,"ticker":"ACME","title":"Acme Widgets Inc."}}"#;

/// Counts requests and remembers the last User-Agent the mock server saw.
#[derive(Clone, Default)]
struct Recorder {
    hits: Arc<AtomicUsize>,
    user_agent: Arc<Mutex<Option<String>>>,
}

async fn ticker(State(state): State<Recorder>, headers: HeaderMap) -> &'static str {
    state.hits.fetch_add(1, Ordering::SeqCst);
    let seen = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    *state.user_agent.lock().expect("mock state lock") = seen;
    TICKER_JSON
}

async fn refused(State(state): State<Recorder>) -> (StatusCode, &'static str) {
    state.hits.fetch_add(1, Ordering::SeqCst);
    (StatusCode::TOO_MANY_REQUESTS, "slow down")
}

async fn oversized(State(state): State<Recorder>) -> Vec<u8> {
    state.hits.fetch_add(1, Ordering::SeqCst);
    vec![b'x'; MAX_SEC_RESPONSE_BYTES + 1]
}

/// Bind an ephemeral port and return the base URL the client should use.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let address = listener.local_addr().expect("mock server address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

fn client(base: &str, user_agent: &str) -> EdgarClient {
    EdgarClient::with_base(user_agent, base, base)
}

#[tokio::test]
async fn sends_the_declared_user_agent() {
    let recorder = Recorder::default();
    let app = Router::new()
        .route("/files/company_tickers.json", get(ticker))
        .with_state(recorder.clone());
    let base = serve(app).await;
    let client = client(&base, "sail-test test@example.com");

    let index = client.ticker_index().await.expect("ticker index");

    assert_eq!(index.entries.len(), 1);
    assert_eq!(recorder.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        recorder
            .user_agent
            .lock()
            .expect("mock state lock")
            .as_deref(),
        Some("sail-test test@example.com")
    );
}

#[tokio::test]
async fn not_configured_makes_no_request() {
    let recorder = Recorder::default();
    let app = Router::new()
        .route("/files/company_tickers.json", get(ticker))
        .with_state(recorder.clone());
    let base = serve(app).await;
    let client = client(&base, "   ");

    assert!(!client.is_configured());
    assert_eq!(client.ticker_index().await, Err(SourceError::NotConfigured));
    assert_eq!(recorder.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rate_limit_refusal_maps_to_refused() {
    let recorder = Recorder::default();
    let app = Router::new()
        .route("/files/company_tickers.json", get(refused))
        .with_state(recorder.clone());
    let base = serve(app).await;
    let client = client(&base, "sail-test test@example.com");

    assert_eq!(client.ticker_index().await, Err(SourceError::Refused(429)));
    assert_eq!(recorder.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_oversized_body_maps_to_too_large() {
    let recorder = Recorder::default();
    let app = Router::new()
        .route("/files/company_tickers.json", get(oversized))
        .with_state(recorder.clone());
    let base = serve(app).await;
    let client = client(&base, "sail-test test@example.com");

    assert_eq!(client.ticker_index().await, Err(SourceError::TooLarge));
    assert_eq!(recorder.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn clones_share_the_rate_limit() {
    let recorder = Recorder::default();
    let app = Router::new()
        .route("/files/company_tickers.json", get(ticker))
        .with_state(recorder.clone());
    let base = serve(app).await;
    let client = client(&base, "sail-test test@example.com");

    let started = Instant::now();
    client.clone().ticker_index().await.expect("first call");
    client.clone().ticker_index().await.expect("second call");
    client.ticker_index().await.expect("third call");
    let elapsed = started.elapsed();

    assert_eq!(recorder.hits.load(Ordering::SeqCst), 3);
    assert!(
        elapsed >= Duration::from_millis(950),
        "three calls took {elapsed:?}, expected about one second"
    );
}
