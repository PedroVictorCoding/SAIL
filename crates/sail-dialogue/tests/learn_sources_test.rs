//! Practice, imports and opt-in Pulse reminders over the owner HTTP API (#244, #250, #257).
use sail_arena::MemoryStore;
use sail_arena_learn::agent::ArenaKnowledge;
use sail_arena_learn::{Engine, Model, SqliteEventStore};
use sail_protocol::{
    FunctionCall, InferenceRequest, InferenceResponse, Message, Role, Timings, ToolCall, Usage,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "learn-sources-owner-token-0123456789";

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

fn call(name: &str, args: Value) -> Result<Message, String> {
    let mut m = Message::text(Role::Assistant, "");
    m.tool_calls = vec![ToolCall {
        id: "c".into(),
        kind: "function".into(),
        function: FunctionCall {
            name: name.into(),
            arguments: args.to_string(),
        },
    }];
    Ok(m)
}

fn extractor_empty() -> Result<Message, String> {
    call("extract_propositions", json!({"propositions":[]}))
}

async fn send(req: reqwest::RequestBuilder) -> (u16, Value) {
    let r = req.bearer_auth(TOKEN).send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

async fn wait(base: &str, id: &str, ready: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..400 {
        let (_, view) = send(reqwest::Client::new().get(format!("{base}/sessions/{id}"))).await;
        if ready(&view) {
            return view;
        }
        last = view;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("view never became ready: {last}");
}

#[tokio::test]
async fn practice_hints_conclusion_imports_and_reminders() {
    let model = Arc::new(Scripted(Mutex::new(
        vec![
            call(
                "problem",
                json!({"statement":"What happens after loss?","solution":"The window halves.",
                       "hints":["Think about the window."]}),
            ),
            call("conclude", json!({"outcome":"solved_with_hints"})),
            extractor_empty(),
            extractor_empty(),
        ]
        .into(),
    )));
    let store = Arc::new(SqliteEventStore::open_in_memory().unwrap());
    let knowledge = ArenaKnowledge::new(Arc::new(MemoryStore::open_in_memory().unwrap())).unwrap();
    let engine = Arc::new(
        Engine::new(store, model)
            .with_knowledge(Arc::new(knowledge))
            .with_prompts(None),
    );
    let pulse = Arc::new(sail_pulse::Pulse::open_in_memory().unwrap());
    let app = axum::Router::new().nest_service(
        "/api/learn",
        sail_dialogue::learn_http::router_with_pulse(
            engine,
            Some(sail_guardian::authority::ApprovalAuthority::new(TOKEN).unwrap()),
            Some(pulse.clone()),
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/learn", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (status, created) = send(
        reqwest::Client::new()
            .post(format!("{base}/sessions"))
            .json(&json!({"goal":"Congestion control","mode":"practice"})),
    )
    .await;
    assert_eq!(status, 201);
    let id = created["session"].as_str().unwrap().to_string();
    let v = wait(&base, &id, |v| {
        v["active_question"].is_null() && v["practice"]["active_problem"].is_string()
    })
    .await;
    let pid = v["practice"]["active_problem"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(v["practice"]["problems"][0]["solution"].is_null());

    let (status, hinted) = send(
        reqwest::Client::new()
            .post(format!("{base}/sessions/{id}/hint"))
            .json(&json!({"problem_id":pid,"index":0})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        hinted["practice"]["problems"][0]["hints"][0],
        "Think about the window."
    );

    let (status, _) = send(
        reqwest::Client::new()
            .post(format!("{base}/sessions/{id}/attempt"))
            .json(&json!({"problem_id":pid,"text":"It halves."})),
    )
    .await;
    assert_eq!(status, 202);
    let v = wait(&base, &id, |v| {
        v["practice"]["problems"][0]["outcome"] == "solved_with_hints"
    })
    .await;
    assert_eq!(
        v["practice"]["problems"][0]["solution"],
        "The window halves."
    );

    // Opt-in reminder at 3 days, then cancel it.
    let (status, scheduled) = send(
        reqwest::Client::new()
            .post(format!("{base}/sessions/{id}/review"))
            .json(&json!({"days":3,"timezone":"Europe/Lisbon","utc_offset_minutes":0})),
    )
    .await;
    assert_eq!(status, 201, "{scheduled}");
    let reminder = scheduled["review"]["reminder_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(scheduled["review"]["days"], 3);
    assert_eq!(
        pulse.get(&reminder).unwrap().status,
        sail_pulse::ReminderStatus::Pending
    );
    assert_eq!(
        pulse.get(&reminder).unwrap().link.as_deref(),
        Some("#arena?practice=Congestion%20control")
    );
    let (status, cancelled) =
        send(reqwest::Client::new().delete(format!("{base}/sessions/{id}/review"))).await;
    assert_eq!(
        (status, cancelled["cancelled"].as_bool()),
        (200, Some(true))
    );

    // Import a Markdown note; extraction runs in the background with the scripted model.
    let (status, uploaded) =
        send(reqwest::Client::new().post(format!("{base}/sources")).json(
            &json!({"topic":"tcp","name":"notes.md","content":"The window halves after loss."}),
        ))
        .await;
    assert_eq!(status, 202, "{uploaded}");
    let source_id = uploaded["source"]["id"].as_i64().unwrap();
    let (status, listed) =
        send(reqwest::Client::new().get(format!("{base}/sources?topic=tcp"))).await;
    assert_eq!(status, 200);
    assert_eq!(listed["sources"][0]["id"], source_id);
    assert!(listed["propositions"].is_number());
    let (status, _) =
        send(reqwest::Client::new().delete(format!("{base}/sources/{source_id}"))).await;
    assert_eq!(status, 200);
    task.abort();
}
