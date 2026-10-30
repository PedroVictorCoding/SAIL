use sail_mind::{GemmaConfig, GemmaRunner};
use serde_json::json;
#[tokio::test]
async fn local_stream_preserves_unicode_and_assembles_tool_calls() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        let header = String::from_utf8_lossy(&request).to_lowercase();
        let length: usize = header
            .lines()
            .find_map(|l| l.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["stream"],
            true
        );
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let events = [
            json!({"choices":[{"delta":{"content":"Olá "}}]}),
            json!({"choices":[{"delta":{"content":"mundo","tool_calls":[{"index":0,"id":"call_1","function":{"name":"os_","arguments":"{"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"status","arguments":"}"}}]},"finish_reason":"tool_calls"}]}),
        ];
        let stream = events
            .iter()
            .map(|v| format!("data: {v}\r\n\r\n"))
            .collect::<String>()
            + "data: [DONE]\r\n\r\n";
        for bytes in stream.as_bytes().chunks(7) {
            socket.write_all(bytes).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    let runner = GemmaRunner::new(GemmaConfig {
        server_url: format!("http://{addr}/v1/chat/completions"),
        ..Default::default()
    });
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let assistant = runner
        .stream_chat(
            vec![json!({"role":"user","content":"hello"})],
            json!([]),
            tx,
        )
        .await
        .unwrap();
    assert_eq!(assistant["content"], "Olá mundo");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "os_status");
    assert_eq!(assistant["tool_calls"][0]["function"]["arguments"], "{}");
    let mut deltas = String::new();
    while let Some(text) = rx.recv().await {
        deltas.push_str(&text);
    }
    assert_eq!(deltas, "Olá mundo");
    server.await.unwrap();
}
#[tokio::test]
async fn voice_rejects_external_inference_endpoints() {
    let runner = GemmaRunner::new(GemmaConfig {
        server_url: "https://example.com/v1/chat/completions".into(),
        ..Default::default()
    });
    let (tx, _) = tokio::sync::mpsc::channel(1);
    assert!(
        runner
            .stream_chat(vec![], json!([]), tx)
            .await
            .unwrap_err()
            .to_string()
            .contains("loopback")
    );
}
