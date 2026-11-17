//! Round 25 B7: the tax-profile route's HTTP surface (owner token, body checks). The
//! profile itself is covered by `sail-capital`'s `dividend_tax` tests.
use serde_json::Value;
use std::sync::Arc;

const TOKEN: &str = "capital-tax-http-owner-token-at-least-32-bytes";

#[tokio::test]
async fn the_tax_route_requires_the_owner_and_rejects_bad_bodies() {
    let dir = std::env::temp_dir().join(format!("capital-tax-http-{}", uuid::Uuid::new_v4()));
    let service = Arc::new(sail_capital::service::CapitalService::open(&dir).unwrap());
    let router = sail_dialogue::capital_http::router(
        service,
        Some(sail_guardian::authority::ApprovalAuthority::new(TOKEN).unwrap()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/dividends/tax", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();

    assert_eq!(client.get(&base).send().await.unwrap().status(), 401);
    let flat = r#"{"w8ben":true,"pt_regime":{"kind":"flat"}}"#;
    assert_eq!(
        client.post(&base).body(flat).send().await.unwrap().status(),
        401
    );

    for (body, status) in [
        ("not json".to_string(), 400),
        (
            r#"{"w8ben":true,"pt_regime":{"kind":"bogus"}}"#.to_string(),
            400,
        ),
        (format!("{{\"pad\":\"{}\"}}", "x".repeat(2000)), 413),
    ] {
        let response = client
            .post(&base)
            .bearer_auth(TOKEN)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{body:.40}");
        let reply: Value = response.json().await.unwrap();
        assert!(reply["error"].is_string(), "{reply}");
    }

    // A valid body reaches the service; an empty store has no account to save it to.
    let response = client
        .post(&base)
        .bearer_auth(TOKEN)
        .body(flat)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    task.abort();
    let _ = task.await;
}
