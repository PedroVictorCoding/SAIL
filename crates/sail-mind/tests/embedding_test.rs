use axum::{Json, Router, routing::post};
use sail_mind::embedding::EmbeddingClient;
async fn worker(value: serde_json::Value) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/embeddings",
        post(move || {
            let value = value.clone();
            async move { Json(value) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/v1/embeddings"), handle)
}
#[tokio::test]
async fn exact_model_and_dimensions_are_required() {
    let (url, task) =
        worker(serde_json::json!({"model":"embed","data":[{"index":0,"embedding":[1.0,0.0]}]}))
            .await;
    let client = EmbeddingClient::new(&url, "embed", "sha256:pinned", 2).unwrap();
    assert_eq!(
        client.embed("local knowledge").await.unwrap().values,
        vec![1., 0.]
    );
    assert!(
        EmbeddingClient::new(&url, "different", "sha256:pinned", 2)
            .unwrap()
            .embed("x")
            .await
            .is_err()
    );
    assert!(
        EmbeddingClient::new(&url, "embed", "sha256:pinned", 3)
            .unwrap()
            .embed("x")
            .await
            .is_err()
    );
    task.abort();
}
#[tokio::test]
async fn invalid_vectors_and_inputs_are_rejected() {
    let (url, task) =
        worker(serde_json::json!({"model":"embed","data":[{"index":0,"embedding":[0.0,0.0]}]}))
            .await;
    let client = EmbeddingClient::new(&url, "embed", "pinned", 2).unwrap();
    assert!(client.embed("x").await.is_err());
    assert!(client.embed("").await.is_err());
    task.abort();
    assert!(EmbeddingClient::new("file:///tmp/model", "embed", "pinned", 2).is_err());
    assert!(EmbeddingClient::new("http://localhost", "embed", "", 2).is_err());
}
