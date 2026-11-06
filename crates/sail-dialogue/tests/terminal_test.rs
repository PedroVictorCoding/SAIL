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
        .write(b"stty size; echo SAIL_TERMINAL_ONLINE\n")
        .await
        .expect("Failed to write to terminal");

    let mut accumulated = String::new();
    let read_result = timeout(Duration::from_secs(5), async {
        while let Some(bytes) = output_rx.recv().await {
            let chunk = String::from_utf8_lossy(&bytes);
            accumulated.push_str(&chunk);
            if accumulated.contains("30 100") && accumulated.contains("SAIL_TERMINAL_ONLINE") {
                return true;
            }
        }
        false
    })
    .await;

    assert!(
        read_result.unwrap_or(false),
        "Expected output containing '30 100' and 'SAIL_TERMINAL_ONLINE', got: {}",
        accumulated
    );

    session.kill();
    let dead = timeout(Duration::from_secs(2), async {
        while session.is_alive() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        dead.is_ok(),
        "Expected session to no longer be alive after kill"
    );
    assert!(!session.is_alive());
}

#[tokio::test]
async fn test_terminal_spawn_piped() {
    let cwd = std::env::current_dir().unwrap();
    let cwd_str = cwd.to_string_lossy().to_string();
    let (session, mut output_rx) = TerminalSession::spawn_piped("/bin/sh", &cwd, &cwd_str)
        .expect("Failed to spawn piped terminal");
    assert!(session.is_alive());

    session
        .write(b"echo PIPED_OUT; echo PIPED_ERR >&2\n")
        .await
        .expect("Failed to write to piped terminal");

    let mut accumulated = String::new();
    let read_result = timeout(Duration::from_secs(5), async {
        while let Some(bytes) = output_rx.recv().await {
            accumulated.push_str(&String::from_utf8_lossy(&bytes));
            if accumulated.contains("PIPED_OUT") && accumulated.contains("PIPED_ERR") {
                return true;
            }
        }
        false
    })
    .await;

    assert!(
        read_result.unwrap_or(false),
        "Expected PIPED_OUT and PIPED_ERR in piped output, got: {}",
        accumulated
    );

    session.kill();
    let dead = timeout(Duration::from_secs(2), async {
        while session.is_alive() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        dead.is_ok(),
        "Expected piped session to no longer be alive after kill"
    );
    assert!(!session.is_alive());
}

const TEST_OWNER_TOKEN: &str = "terminal-test-owner-token-0123456789abcdef";

async fn test_ws_terminal(_headers: HeaderMap, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|socket| {
        let guard = sail_dialogue::terminal::TerminalGuard::new(
            Some(TEST_OWNER_TOKEN.to_string()),
            "test",
            None,
        );
        sail_dialogue::terminal::handle_terminal_socket(socket, None, guard)
    })
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
    assert!(js_body.contains("new Terminal("));

    // 2. Connect to WebSocket /ws/terminal
    let ws_url = format!("ws://{addr}/ws/terminal");
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("Failed to connect to /ws/terminal");

    // The shell opens only after the socket proves it is the owner.
    let auth = serde_json::to_string(&TerminalClientMessage::Auth {
        token: TEST_OWNER_TOKEN.to_string(),
    })
    .unwrap();
    ws_stream
        .send(Message::Text(auth))
        .await
        .expect("Failed to send auth");

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
            if let Some(data) = terminal_output(msg) {
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

    // 6. Test Message::Binary input
    ws_stream
        .send(Message::Binary(b"echo BINARY_WS_OK\n".to_vec()))
        .await
        .expect("Failed to send binary input");

    let mut bin_accumulated = String::new();
    let bin_verified = timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            if let Some(data) = terminal_output(msg) {
                bin_accumulated.push_str(&data);
                if bin_accumulated.contains("BINARY_WS_OK") {
                    return true;
                }
            }
        }
        false
    })
    .await;

    assert!(
        bin_verified.unwrap_or(false),
        "Expected output containing BINARY_WS_OK, got: {}",
        bin_accumulated
    );

    // Exit must be reported without a further client keystroke, after final bytes.
    ws_stream.send(Message::Text(serde_json::to_string(&TerminalClientMessage::Input {
        data: "printf '\\303'; sleep 0.05; printf '\\251'; printf 'FINAL_%s' RECEIPT; exit 7\n".into(),
    }).unwrap())).await.unwrap();
    let (bytes, code) = timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        while let Some(Ok(message)) = ws_stream.next().await {
            match message {
                Message::Binary(chunk) => bytes.extend(chunk),
                Message::Text(text) => {
                    if let Ok(TerminalServerMessage::Exit { code }) = serde_json::from_str(&text) {
                        return (bytes, code);
                    }
                }
                _ => {}
            }
        }
        panic!("socket closed without an exit receipt");
    })
    .await
    .expect("shell exit must be reported promptly");
    let text = String::from_utf8(bytes).expect("PTY output must preserve split UTF-8 bytes");
    assert!(text.contains("éFINAL_RECEIPT"), "{text}");
    assert_eq!(code, Some(7));

    // 7. Test Message::Close frame
    ws_stream
        .send(Message::Close(None))
        .await
        .expect("Failed to send close frame");

    let closed = timeout(Duration::from_secs(5), async {
        while let Some(msg) = ws_stream.next().await {
            if msg.is_err() || matches!(msg.ok(), Some(Message::Close(_))) {
                return true;
            }
        }
        true
    })
    .await;
    assert!(closed.unwrap_or(false), "Expected socket to close");

    server_task.abort();
}

fn terminal_output(msg: Message) -> Option<String> {
    match msg {
        Message::Binary(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Message::Text(text) => match serde_json::from_str(&text).ok()? {
            TerminalServerMessage::Output { data } => Some(data),
            _ => None,
        },
        _ => None,
    }
}

#[tokio::test]
async fn piped_exit_drains_both_streams() {
    let cwd = std::env::current_dir().unwrap();
    let (session, mut output) = TerminalSession::spawn_piped("/bin/sh", &cwd, ".").unwrap();
    session
        .write(b"printf 'final stdout'; printf 'final stderr' >&2; exit 7\n")
        .await
        .unwrap();
    let bytes = timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        while let Some(chunk) = output.recv().await {
            bytes.extend(chunk);
        }
        bytes
    })
    .await
    .expect("output must close without additional input");
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("final stdout"), "{text}");
    assert!(text.contains("final stderr"), "{text}");
}
