//! A minimal Chrome DevTools Protocol client over the browser's loopback WebSocket.
//!
//! One connection carries every target through flattened sessions (`sessionId`). Commands
//! are answered by id; everything else is an event, fanned out on a broadcast channel.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use super::BrowserError;

/// A CDP event: `method`, `params` and the session it belongs to (`None` for the browser).
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub session_id: Option<String>,
    pub method: String,
    pub params: Value,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, BrowserError>>>>>;

#[derive(Clone)]
pub struct CdpConnection {
    outgoing: mpsc::UnboundedSender<String>,
    pending: Pending,
    events: broadcast::Sender<CdpEvent>,
    next_id: Arc<AtomicU64>,
    timeout: Duration,
}

impl CdpConnection {
    /// Connect to a `ws://127.0.0.1:<port>/devtools/browser/<id>` endpoint.
    pub async fn connect(endpoint: &str, timeout: Duration) -> Result<Self, BrowserError> {
        let parsed = url::Url::parse(endpoint)
            .map_err(|e| BrowserError::Protocol(format!("bad DevTools endpoint: {e}")))?;
        // The DevTools socket is full control of the browser; never follow it off the host.
        if parsed.scheme() != "ws" || parsed.host_str() != Some("127.0.0.1") {
            return Err(BrowserError::Protocol(format!(
                "DevTools endpoint must be ws://127.0.0.1, got {endpoint}"
            )));
        }
        let (socket, _) = tokio_tungstenite::connect_async(endpoint)
            .await
            .map_err(|e| BrowserError::Protocol(format!("DevTools connect failed: {e}")))?;
        let (mut sink, mut stream) = socket.split();
        let (outgoing, mut outgoing_rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::default();
        let (events, _) = broadcast::channel(1024);

        tokio::spawn(async move {
            while let Some(text) = outgoing_rx.recv().await {
                if sink.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
        });

        let reader_pending = pending.clone();
        let reader_events = events.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    let waiter = reader_pending.lock().expect("cdp pending").remove(&id);
                    if let Some(waiter) = waiter {
                        let result = match value.get("error") {
                            Some(error) => Err(BrowserError::Protocol(error.to_string())),
                            None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = waiter.send(result);
                    }
                } else if let Some(method) = value.get("method").and_then(Value::as_str) {
                    let _ = reader_events.send(CdpEvent {
                        session_id: value
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        method: method.to_owned(),
                        params: value.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            // Socket closed: fail every outstanding command instead of leaving it hanging.
            for (_, waiter) in reader_pending.lock().expect("cdp pending").drain() {
                let _ = waiter.send(Err(BrowserError::Closed));
            }
        });

        Ok(Self {
            outgoing,
            pending,
            events,
            next_id: Arc::new(AtomicU64::new(1)),
            timeout,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Send a command and wait for its result.
    pub async fn call(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, BrowserError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session_id) = session_id {
            message["sessionId"] = json!(session_id);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("cdp pending").insert(id, tx);
        if self.outgoing.send(message.to_string()).is_err() {
            self.pending.lock().expect("cdp pending").remove(&id);
            return Err(BrowserError::Closed);
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BrowserError::Closed),
            Err(_) => {
                self.pending.lock().expect("cdp pending").remove(&id);
                Err(BrowserError::Timeout(format!("CDP {method}")))
            }
        }
    }

    /// Fire a command without waiting (used from the event loop, which must not block).
    pub fn send(&self, session_id: Option<&str>, method: &str, params: Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session_id) = session_id {
            message["sessionId"] = json!(session_id);
        }
        let _ = self.outgoing.send(message.to_string());
    }
}
