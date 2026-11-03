use axum::{
    Router, extract::ws::WebSocketUpgrade, http::HeaderMap, response::IntoResponse, routing::get,
};
use futures_util::{SinkExt, StreamExt};
use sail_dialogue::terminal::{TerminalClientMessage, TerminalServerMessage, TerminalSession};
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::protocol::Message;

#[tokio::test]
async fn test_terminal_session_lifecycle() {
    let (session, mut output_rx) = TerminalSession::spawn(None).expect("Failed to spawn terminal");
    assert!(session.is_alive());

    session.resize(100, 30);

    session
        .write(b"echo SAIL_TERMINAL_ONLINE\n")
        .await
        .expect("Failed to write to terminal");

    let mut accumulated = String::new();
    let read_result = timeout(Duration::from_secs(5), async {
        while let Some(bytes) = output_rx.recv().await {
            let chunk = String::from_utf8_lossy(&bytes);
            accumulated.push_str(&chunk);
            if accumulated.contains("SAIL_TERMINAL_ONLINE") {
                return true;
            }
        }
        false
    })
    .await;

    assert!(
        read_result.unwrap_or(false),
        "Expected output containing SAIL_TERMINAL_ONLINE, got: {}",
        accumulated
    );

    session.kill();
}

async fn test_ws_terminal(_headers: HeaderMap, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|socket| sail_dialogue::terminal::handle_terminal_socket(socket, None))
}

#[tokio::test]
async fn test_terminal_websocket_integration() {
    let app = Router::new()
        .route(
            "/terminal.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../web/terminal.js"),
                )
            }),
        )
        .route("/ws/terminal", get(test_ws_terminal));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // 1. Verify GET /terminal.js returns the terminal frontend bundle
    let client = reqwest::Client::new();
    let js_res = client
        .get(format!("http://{addr}/terminal.js"))
        .send()
        .await
        .expect("Failed to fetch /terminal.js");
    assert!(js_res.status().is_success());
    let js_body = js_res.text().await.unwrap();
    assert!(js_body.contains("TerminalApp"));
    assert!(js_body.contains("AnsiParser"));

    // 2. Connect to WebSocket /ws/terminal
    let ws_url = format!("ws://{addr}/ws/terminal");
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("Failed to connect to /ws/terminal");

    // First message must be "connected"
    let first_msg = timeout(Duration::from_secs(5), ws_stream.next())
        .await
        .expect("Timeout waiting for handshake")
        .expect("Stream ended")
        .expect("WS error");

    let first_text = first_msg.to_text().expect("Text frame expected");
    let handshake: TerminalServerMessage =
        serde_json::from_str(first_text).expect("Valid JSON handshake");
    match handshake {
        TerminalServerMessage::Connected { shell, cwd } => {
            assert!(!shell.is_empty());
            assert!(!cwd.is_empty());
        }
        other => panic!("Expected Connected message, got: {:?}", other),
    }

    // 3. Send resize message
    let resize_msg = serde_json::to_string(&TerminalClientMessage::Resize {
        cols: 120,
        rows: 35,
    })
    .unwrap();
    ws_stream
        .send(Message::Text(resize_msg))
        .await
        .expect("Failed to send resize");

    // 4. Send shell command input
    let cmd_msg = serde_json::to_string(&TerminalClientMessage::Input {
        data: "echo PTY_WS_VERIFIED\n".into(),
    })
    .unwrap();
    ws_stream
        .send(Message::Text(cmd_msg))
        .await
        .expect("Failed to send input");

    // Accumulate output until PTY_WS_VERIFIED is received
    let mut accumulated = String::new();
    let verified = timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            if let Ok(text) = msg.to_text()
                && let Ok(TerminalServerMessage::Output { data }) =
                    serde_json::from_str::<TerminalServerMessage>(text)
            {
                accumulated.push_str(&data);
                if accumulated.contains("PTY_WS_VERIFIED") {
                    return true;
                }
            }
        }
        false
    })
    .await;

    assert!(
        verified.unwrap_or(false),
        "Expected output containing PTY_WS_VERIFIED, got: {}",
        accumulated
    );

    // 5. Send restart command and verify new connection handshake
    let restart_msg = serde_json::to_string(&TerminalClientMessage::Restart).unwrap();
    ws_stream
        .send(Message::Text(restart_msg))
        .await
        .expect("Failed to send restart");

    let restarted = timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            if let Ok(text) = msg.to_text()
                && let Ok(TerminalServerMessage::Connected { .. }) =
                    serde_json::from_str::<TerminalServerMessage>(text)
            {
                return true;
            }
        }
        false
    })
    .await;

    assert!(
        restarted.unwrap_or(false),
        "Expected reconnected handshake after restart"
    );

    // Clean up
    let _ = ws_stream.close(None).await;
    server_task.abort();
}
