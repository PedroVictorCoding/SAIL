//! Arena Learn L1 acceptance (#258): the whole loop over real HTTP and an on-disk store,
//! with a scripted model. No network or GPU needed.
use sail_arena_learn::{Engine, Model, SqliteEventStore};
use sail_protocol::{
    FunctionCall, InferenceRequest, InferenceResponse, Message, Role, Timings, ToolCall, Usage,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "learn-acceptance-owner-token-0123456789";

struct Scripted(Mutex<VecDeque<Result<Message, String>>>);

#[async_trait::async_trait]
impl Model for Scripted {
    async fn complete(
        &self,
        request: InferenceRequest,
        _: CancellationToken,
    ) -> Result<InferenceResponse, String> {
        sail_mind::validate_request(&request, &sail_protocol::ResourceLimits::default())
            .map_err(|e| e.message)?;
        let message = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or("script exhausted")??;
        Ok(InferenceResponse {
            version: sail_protocol::VERSION,
            request_id: request.request_id,
            model: "scripted".into(),
            message,
            finish_reason: "stop".into(),
            usage: Usage::default(),
            timings: Timings {
                queue_ms: 0,
                first_token_ms: None,
                inference_ms: 1,
            },
        })
    }
}

fn tool(calls: &[(&str, Value)]) -> Result<Message, String> {
    let mut m = Message::text(Role::Assistant, "");
    m.tool_calls = calls
        .iter()
        .map(|(name, args)| ToolCall {
            id: "c".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        })
        .collect();
    Ok(m)
}

fn probe(prompt: &str) -> Value {
    json!({"kind":"single","prompt":prompt,"options":[{"id":"y","text":"Yes"},{"id":"n","text":"No"},
           {"id":"idk","text":"I don't know"}],"correct":["y"]})
}

struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

async fn serve(path: &std::path::Path, script: Vec<Result<Message, String>>) -> Server {
    let store = Arc::new(SqliteEventStore::open(path).unwrap());
    let engine = Arc::new(Engine::new(
        store,
        Arc::new(Scripted(Mutex::new(script.into()))),
    ));
    let app = axum::Router::new().nest_service(
        "/api/learn",
        sail_dialogue::learn_http::router(
            engine,
            Some(sail_guardian::authority::ApprovalAuthority::new(TOKEN).unwrap()),
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/learn", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { base, task }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn send(req: reqwest::RequestBuilder) -> (u16, Value) {
    let r = req.bearer_auth(TOKEN).send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

/// Polls the view until `ready` holds (turns run in the background).
async fn wait(base: &str, id: &str, ready: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..400 {
        let (_, view) = send(client().get(format!("{base}/sessions/{id}"))).await;
        if ready(&view) {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("view never became ready");
}

fn settled_with_question(v: &Value) -> bool {
    v["running"] == false && v["active_question"].is_string()
}

#[tokio::test]
async fn learn_loop_over_http_survives_reconnect_and_restart() {
    let path =
        std::env::temp_dir().join(format!("sail-learn-acceptance-{}.db", uuid::Uuid::new_v4()));
    let script = vec![
        tool(&[("probe", probe("Do you know what a packet is?"))]),
        tool(&[("probe", probe("Have you used TCP sockets?"))]),
        tool(&[(
            "probe_done",
            json!({"summary":"Knows packets and sockets","gaps":["congestion"]}),
        )]),
        tool(&[
            (
                "plan_init",
                json!({"goal":"cc","nodes":[{"id":"loss","title":"Packet loss"},
                {"id":"win","title":"Congestion window"},{"id":"cc","title":"Congestion control"}],
                "edges":[["loss","win"],["win","cc"]]}),
            ),
            (
                "explain",
                json!({"node_id":"loss","text":"Routers drop packets when queues fill."}),
            ),
        ]),
        tool(&[(
            "quiz",
            json!({"node_id":"loss","kind":"free_recall","prompt":"Why do routers drop packets?"}),
        )]),
    ];
    let server = serve(&path, script).await;
    let base = server.base.clone();

    // Owner authentication is required.
    let r = client()
        .get(format!("{base}/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);

    let (status, created) = send(
        client()
            .post(format!("{base}/sessions"))
            .json(&json!({"goal":"Understand TCP congestion control"})),
    )
    .await;
    assert_eq!(status, 201);
    let id = created["session"].as_str().unwrap().to_string();

    // The live stream sends the view at once; dropping it (a disconnect) does not stop the turn.
    let mut stream = client()
        .get(format!("{base}/sessions/{id}/stream"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status().as_u16(), 200);
    let chunk = String::from_utf8(stream.chunk().await.unwrap().unwrap().to_vec()).unwrap();
    assert!(chunk.starts_with("event: view"), "{chunk}");
    drop(stream);

    // Probe 1: answer it, then retry the same submission as a reconnecting client would.
    let v = wait(&base, &id, settled_with_question).await;
    let q1 = v["active_question"].as_str().unwrap().to_string();
    assert!(v["transcript"].to_string().contains("packet"));
    let body = json!({"question_id":q1,"answer":{"choices":["y"],"confidence":3}});
    let (status, first) = send(
        client()
            .post(format!("{base}/sessions/{id}/answer"))
            .json(&body),
    )
    .await;
    assert_eq!(
        (
            status,
            first["grade"].as_str(),
            first["duplicate"].as_bool()
        ),
        (200, Some("correct"), Some(false))
    );
    let v = wait(&base, &id, |v| {
        settled_with_question(v) && v["active_question"] != json!(q1)
    })
    .await;
    let (status, again) = send(
        client()
            .post(format!("{base}/sessions/{id}/answer"))
            .json(&body),
    )
    .await;
    assert_eq!((status, again["duplicate"].as_bool()), (200, Some(true)));

    // Probe 2, then probe_done → teacher: plan, explanation and a node-tagged quiz.
    let q2 = v["active_question"].as_str().unwrap().to_string();
    let (status, _) = send(
        client()
            .post(format!("{base}/sessions/{id}/answer"))
            .json(&json!({"question_id":q2,"answer":{"choices":["n"]}})),
    )
    .await;
    assert_eq!(status, 200);
    let v = wait(&base, &id, |v| {
        settled_with_question(v) && v["mode"] == "teach"
    })
    .await;
    assert_eq!(v["plan"]["order"], json!(["loss", "win", "cc"]));
    assert!(
        v["plan"]["mermaid"]
            .as_str()
            .unwrap()
            .contains("loss --> win")
    );
    assert_eq!(v["progress"]["visited"], json!(["loss"]));
    assert_eq!(v["needs_turn"], false);
    let quiz = v["active_question"].as_str().unwrap().to_string();
    // A turn can't start while a question is open.
    let (status, _) = send(client().post(format!("{base}/sessions/{id}/turn"))).await;
    assert_eq!(status, 409);

    // Pause, then restart the process on the same database.
    let (status, paused) = send(
        client()
            .post(format!("{base}/sessions/{id}/pause"))
            .json(&json!({"paused":true})),
    )
    .await;
    assert_eq!((status, paused["paused"].as_bool()), (200, Some(true)));
    let before = wait(&base, &id, |_| true).await;
    server.task.abort();

    let server = serve(&path, vec![Err("model offline".into())]).await;
    let base = server.base.clone();
    let (_, sessions) = send(client().get(format!("{base}/sessions"))).await;
    assert_eq!(sessions["sessions"][0]["id"], json!(id));
    let after = wait(&base, &id, |_| true).await;
    assert_eq!(after["transcript"], before["transcript"]);
    assert_eq!(after["paused"], true);
    let (status, _) = send(
        client()
            .post(format!("{base}/sessions/{id}/pause"))
            .json(&json!({"paused":false})),
    )
    .await;
    assert_eq!(status, 200);

    // Answer the quiz; the model is offline, so the failure is reported, not papered over.
    let (status, graded) = send(client().post(format!("{base}/sessions/{id}/answer"))
        .json(&json!({"question_id":quiz,"answer":{"text":"Their queues overflow","thinking":"buffers are finite"}}))).await;
    assert_eq!(
        (status, graded["grade"].as_str()),
        (200, Some("needs_judgment"))
    );
    let v = wait(&base, &id, |v| {
        v["running"] == false && v["last_failure"].is_string()
    })
    .await;
    assert_eq!(v["last_failure"], "model offline");
    assert_eq!(v["needs_turn"], false);
    let transcript = v["transcript"].as_array().unwrap();
    assert!(
        !transcript.iter().any(|e| e["type"] == "assistant"),
        "no fabricated tutor reply"
    );
    let answered = transcript.iter().find(|e| e["id"] == json!(quiz)).unwrap();
    assert_eq!(answered["answer"]["text"], "Their queues overflow");

    let (status, _) = send(client().delete(format!("{base}/sessions/{id}"))).await;
    assert_eq!(status, 200);
    let (status, _) = send(client().get(format!("{base}/sessions/{id}"))).await;
    assert_eq!(status, 404);
    server.task.abort();
    let _ = std::fs::remove_file(&path);
}
