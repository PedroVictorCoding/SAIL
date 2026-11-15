//! Owner authentication and availability for the early-retirement routes. The plan itself
//! is covered by `sail-capital`'s pure tests; this checks the HTTP surface.
use serde_json::Value;
use std::sync::Arc;

const TOKEN: &str = "capital-retirement-owner-token-at-least-32-bytes";

#[tokio::test]
async fn retirement_routes_require_the_owner_and_report_a_missing_model() {
    let dir =
        std::env::temp_dir().join(format!("capital-retirement-http-{}", uuid::Uuid::new_v4()));
    let service = Arc::new(sail_capital::service::CapitalService::open(&dir).unwrap());
    let router = sail_dialogue::capital_http::router(
        service,
        Some(sail_guardian::authority::ApprovalAuthority::new(TOKEN).unwrap()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();

    for path in ["/dividends/retirement", "/dividends/retirement/research"] {
        let url = format!("{base}{path}");
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            401,
            "{path}"
        );
        assert_eq!(
            client
                .get(&url)
                .bearer_auth("wrong-token")
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "{path}"
        );
    }

    // Without the research engine both research verbs are honest 503s, not empty briefs.
    for method in ["GET", "POST"] {
        let url = format!("{base}/dividends/retirement/research");
        let response = client
            .request(method.parse().unwrap(), &url)
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(503, response.status(), "{method}");
        let body: Value = response.json().await.unwrap();
        assert!(
            body["error"].as_str().unwrap().contains("local SAIL model"),
            "{body}"
        );
    }

    // The plan route is wired and reaches the service (an empty store has no account).
    let response = client
        .get(format!("{base}/dividends/retirement"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(404, response.status());
    let body: Value = response.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("No portfolio yet"));
    task.abort();
    let _ = task.await;
}
