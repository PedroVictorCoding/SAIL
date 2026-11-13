//! Owner authentication and offline input validation for the stock forecast API.
use serde_json::{Value, json};
use std::sync::Arc;
const TOKEN: &str = "capital-prediction-owner-token-at-least-32-bytes";

#[tokio::test]
async fn stock_forecast_requires_owner_and_validates_before_network_work() {
    let dir =
        std::env::temp_dir().join(format!("capital-prediction-http-{}", uuid::Uuid::new_v4()));
    let service = Arc::new(sail_capital::service::CapitalService::open(&dir).unwrap());
    let router = sail_dialogue::capital_http::router(
        service,
        Some(sail_guardian::authority::ApprovalAuthority::new(TOKEN).unwrap()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let url = format!("{base}/prediction");
    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .get(&url)
            .bearer_auth("wrong-token")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client.get(&url).bearer_auth(TOKEN).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status: Value = response.json().await.unwrap();
    assert!(status["job"].is_null());
    assert_eq!(status["model"], sail_capital::prediction::MODEL);
    let request = json!({"symbol":"../../secret","period":"1y","interval":"1d","horizon":20});
    assert_eq!(
        client
            .post(&url)
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .post(&url)
        .bearer_auth(TOKEN)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(
        response.json::<Value>().await.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("ticker")
    );
    let extra = json!({"symbol":"AAPL","period":"1y","interval":"1d","horizon":20,"endpoint":"https://example.com"});
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(TOKEN)
            .json(&extra)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    assert!(!dir.join("predictions/latest.json").exists());
    task.abort();
    std::fs::remove_dir_all(dir).unwrap();
}
