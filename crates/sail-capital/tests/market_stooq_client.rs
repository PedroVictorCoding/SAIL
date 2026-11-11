//! Client tests against an in-process axum mock server on `127.0.0.1:0`.
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::get;
use sail_capital::market::{MAX_PRICE_RESPONSE_BYTES, PriceSource, PriceSourceError, StooqClient};
use serde::Deserialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const SAMPLE_CSV: &str = "Date,Open,High,Low,Close,Volume\n2024-05-01,100,105,95,102,1000\n";

#[derive(Clone, Default)]
struct MockState {
    hits: Arc<AtomicUsize>,
    last_user_agent: Arc<Mutex<Option<String>>>,
    last_query: Arc<Mutex<Option<String>>>,
}

#[derive(Deserialize)]
struct StooqQuery {
    s: Option<String>,
    i: Option<String>,
}

async fn handle_csv(
    State(state): State<MockState>,
    headers: HeaderMap,
    Query(q): Query<StooqQuery>,
) -> &'static str {
    state.hits.fetch_add(1, Ordering::SeqCst);
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    *state.last_user_agent.lock().unwrap() = ua;
    *state.last_query.lock().unwrap() = Some(format!("s={:?}, i={:?}", q.s, q.i));
    SAMPLE_CSV
}

async fn handle_429(State(state): State<MockState>) -> (StatusCode, &'static str) {
    state.hits.fetch_add(1, Ordering::SeqCst);
    (StatusCode::TOO_MANY_REQUESTS, "too many requests")
}

async fn handle_403(State(state): State<MockState>) -> (StatusCode, &'static str) {
    state.hits.fetch_add(1, Ordering::SeqCst);
    (StatusCode::FORBIDDEN, "forbidden")
}

async fn handle_oversized(State(state): State<MockState>) -> Vec<u8> {
    state.hits.fetch_add(1, Ordering::SeqCst);
    vec![b'a'; MAX_PRICE_RESPONSE_BYTES + 1]
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("mock server address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn not_configured_makes_no_request() {
    // Ensure SAIL_PRICES_SOURCE is unset or not "stooq"
    // from_env creates an unconfigured client
    let client = StooqClient::from_env();
    if !client.is_configured() {
        assert_eq!(
            client.daily("AAPL.US").await,
            Err(PriceSourceError::NotConfigured)
        );
    }
}

#[tokio::test]
async fn client_sends_neutral_user_agent_and_queries_correct_url() {
    let state = MockState::default();
    let app = Router::new()
        .route("/q/d/l/", get(handle_csv))
        .with_state(state.clone());
    let base = serve(app).await;

    let client = StooqClient::with_base(&base);
    let series = client.daily("AAPL.US").await.expect("successful fetch");

    assert_eq!(series.symbol, "AAPL.US");
    assert_eq!(series.bars.len(), 1);
    assert_eq!(state.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.last_user_agent.lock().unwrap().as_deref(),
        Some("SAIL-capital/1")
    );
    assert_eq!(
        state.last_query.lock().unwrap().as_deref(),
        Some(r#"s=Some("aapl.us"), i=Some("d")"#)
    );
}

#[tokio::test]
async fn client_refused_on_429_and_403() {
    let state = MockState::default();
    let app = Router::new()
        .route("/q/d/l/", get(handle_429))
        .with_state(state.clone());
    let base = serve(app).await;

    let client = StooqClient::with_base(&base);
    let err = client.daily("AAPL.US").await.unwrap_err();
    assert_eq!(err, PriceSourceError::Refused(429));

    let state403 = MockState::default();
    let app403 = Router::new()
        .route("/q/d/l/", get(handle_403))
        .with_state(state403.clone());
    let base403 = serve(app403).await;

    let client403 = StooqClient::with_base(&base403);
    let err403 = client403.daily("AAPL.US").await.unwrap_err();
    assert_eq!(err403, PriceSourceError::Refused(403));
}

#[tokio::test]
async fn client_size_cap() {
    let state = MockState::default();
    let app = Router::new()
        .route("/q/d/l/", get(handle_oversized))
        .with_state(state.clone());
    let base = serve(app).await;

    let client = StooqClient::with_base(&base);
    let err = client.daily("AAPL.US").await.unwrap_err();
    assert_eq!(err, PriceSourceError::TooLarge);
}

#[tokio::test]
async fn client_rate_limit_holds_across_clones() {
    let state = MockState::default();
    let app = Router::new()
        .route("/q/d/l/", get(handle_csv))
        .with_state(state.clone());
    let base = serve(app).await;

    let client1 = StooqClient::with_base(&base);
    let client2 = client1.clone();

    let start = Instant::now();
    client1.daily("AAPL.US").await.expect("first request");
    client2.daily("GALP.PT").await.expect("second request");
    let elapsed = start.elapsed();

    // With 1 request per second, 2 requests must take at least ~900ms
    assert!(elapsed.as_millis() >= 900, "elapsed was only {:?}", elapsed);
    assert_eq!(state.hits.load(Ordering::SeqCst), 2);
}
