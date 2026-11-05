use super::*;
struct FixtureHost;
impl sail_common::resources::HostProbe for FixtureHost {
    fn sample(&self) -> std::io::Result<sail_protocol::resources::HostSample> {
        use sail_common::resources::GIB;
        Ok(sail_protocol::resources::HostSample {
            available_ram_bytes: 24 * GIB,
            total_ram_bytes: 32 * GIB,
            swap_used_bytes: 0,
            swap_total_bytes: 8 * GIB,
            logical_cpus: 12,
            memory_pressure_avg10: None,
        })
    }
}
/// Opt-in acoustic smoke test with installed offline Whisper and a supplied,
/// consented 16 kHz little-endian PCM sample. Never downloads models or opens a mic.
#[tokio::test]
#[ignore = "requires installed local speech runtime and SAIL_TEST_CONTEXT_PCM"]
async fn live_local_audio_smoke() {
    let bytes = std::fs::read(
        std::env::var("SAIL_TEST_CONTEXT_PCM").expect("supply a synthetic or consented PCM sample"),
    )
    .unwrap();
    assert_eq!(bytes.len() % 2, 0);
    let audio = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let services = VoiceModelServices::new();
    let arena = Arc::new(sail_arena::ArenaSubsystem::new());
    arena.begin_voice_session("acoustic-smoke");
    let state = Arc::new(ServerState {
        bus: EventBus::new(),
        services: services.clone(),
        arena: arena.clone(),
        inference: sail_mind::MindService::from_env().unwrap(),
        live_context_authority: None,
        plans: None,
        pulse: None,
    });
    let context = Arc::new(std::sync::Mutex::new(
        crate::live_context::LiveContext::default(),
    ));
    context.lock().unwrap().set_enabled(true);
    let generation = context.lock().unwrap().generation();
    let (tx, mut rx) = mpsc::channel(16);
    run_turn(
        state,
        "acoustic-smoke".into(),
        1,
        tx,
        LocalStreamingStt::with_services(services.clone()),
        LocalNeuralTts::with_services(Default::default(), services),
        Arc::new(VoiceVlm::new("unused-no-inference")),
        TurnInput::Audio(audio, None),
        Arc::new(AtomicU64::new(1)),
        context.clone(),
        generation,
        SpeechOutput::Host,
    )
    .await
    .unwrap();
    let observed = context
        .lock()
        .unwrap()
        .prompt_context(Instant::now())
        .unwrap();
    assert!(observed.to_lowercase().contains("tuesday"), "{observed}");
    let mut count = 0;
    while let Ok(event) = rx.try_recv() {
        assert!(!matches!(
            event.value["type"].as_str(),
            Some("turn_started" | "tool_started" | "audio_chunk" | "speech_text")
        ));
        if event.value["type"] == "live_context_observed" {
            count += 1;
        }
    }
    assert_eq!(count, 1);
    assert!(
        arena
            .conversation_context("acoustic-smoke", "launch")
            .hot
            .is_empty()
    );
}

#[tokio::test]
async fn live_audio_builds_context_without_inference_tools_tts_or_archive() {
    // The supervised worker is configured via environment variables. Run this
    // fixture in its own process so parallel tests cannot inherit those settings.
    if std::env::var_os("SAIL_LIVE_CONTEXT_FIXTURE_CHILD").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "server::live_conversation_tests::live_audio_builds_context_without_inference_tools_tts_or_archive", "--nocapture"])
                .env("SAIL_LIVE_CONTEXT_FIXTURE_CHILD", "1")
                .output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    // Explicit fixture STT, through the real supervised worker and run_turn path.
    // Run this filtered test alone: worker configuration is process environment.
    let path = std::env::temp_dir().join(format!("sail-live-fixture-{}.py", uuid::Uuid::new_v4()));
    std::fs::write(&path, r#"import sys,json
for line in sys.stdin:
    request=json.loads(line)
    assert sys.argv[1] == 'stt', 'listen-only must not run other models'
    if len(request['pcm']) < 8:
        print(json.dumps({'ok':False,'error':'fixture STT unavailable'}),flush=True)
        continue
    print(json.dumps({'ok':True,'result':{'text':'The launch is on Tuesday.','tokens':[]}}),flush=True)
"#).unwrap();
    let old_python = std::env::var_os("SAIL_VOICE_PYTHON");
    let old_worker = std::env::var_os("SAIL_VOICE_WORKER");
    unsafe {
        std::env::set_var("SAIL_VOICE_PYTHON", "/usr/bin/python3");
        std::env::set_var("SAIL_VOICE_WORKER", &path);
    }
    let services = VoiceModelServices::with_scheduler(Arc::new(
        sail_common::resources::ResourceScheduler::new(Arc::new(FixtureHost), Default::default()),
    ));
    let arena = Arc::new(sail_arena::ArenaSubsystem::new());
    arena.begin_voice_session("fixture");
    arena.set_session_learning("fixture", true).unwrap();
    let state = Arc::new(ServerState {
        bus: EventBus::new(),
        services: services.clone(),
        arena: arena.clone(),
        inference: sail_mind::MindService::from_env().unwrap(),
        plans: None,
        live_context_authority: None,
        pulse: None,
    });
    let context = Arc::new(std::sync::Mutex::new(
        crate::live_context::LiveContext::default(),
    ));
    context.lock().unwrap().set_enabled(true);
    let generation = context.lock().unwrap().generation();
    let (tx, mut rx) = mpsc::channel(16);
    let result = run_turn(
        state.clone(),
        "fixture".into(),
        1,
        tx,
        LocalStreamingStt::with_services(services.clone()),
        LocalNeuralTts::with_services(Default::default(), services.clone()),
        Arc::new(VoiceVlm::new("fixture-unused")),
        TurnInput::Audio(vec![1; 16000], arena.learning_epoch("fixture")),
        Arc::new(AtomicU64::new(1)),
        context.clone(),
        generation,
        SpeechOutput::Host,
    )
    .await;
    result.unwrap();
    // A decoder failure must neither fabricate an observation nor clear prior context.
    let (failure_tx, mut failure_rx) = mpsc::channel(16);
    let failure = run_turn(
        state.clone(),
        "fixture".into(),
        2,
        failure_tx,
        LocalStreamingStt::with_services(services.clone()),
        LocalNeuralTts::with_services(Default::default(), services.clone()),
        Arc::new(VoiceVlm::new("fixture-unused")),
        TurnInput::Audio(vec![1], None),
        Arc::new(AtomicU64::new(2)),
        context.clone(),
        generation,
        SpeechOutput::Host,
    )
    .await;
    assert!(failure.is_err());
    assert!(failure_rx.try_recv().is_err());

    // Exercise real WebSocket consent/authentication/reset handling, with fixture workers.
    use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
    async fn receive_type<S>(
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
        wanted: &str,
    ) -> Value
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let message = socket.next().await.unwrap().unwrap();
                if let WsMessage::Text(text) = message {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["type"] == wanted {
                        return value;
                    }
                }
            }
        })
        .await
        .unwrap()
    }
    // A clearly labelled stand-in model records the actual prompt. No local model
    // quality is claimed; this proves context reaches only an explicit question.
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
    let recorded = requests.clone();
    let model_router = Router::new()
            .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
            .route("/props", get(|| async { Json(json!({"modalities":{"vision":true,"tools":true},"default_generation_settings":{"n_ctx":4096}})) }))
            .route("/apply-template", axum::routing::post(|| async { Json(json!({"prompt":"fixture chat"})) }))
            .route("/tokenize", axum::routing::post(|| async { Json(json!({"tokens":[1,2,3,4]})) }))
            .route("/v1/chat/completions", axum::routing::post(move |Json(request): Json<Value>| {
                let recorded = recorded.clone();
                async move {
                    recorded.lock().await.push(request);
                    let event = json!({"choices":[{"delta":{"content":"The launch is on Tuesday."},"finish_reason":"stop"}]});
                    ([("content-type","text/event-stream")], format!("data: {event}\n\ndata: [DONE]\n\n"))
                }
            }));
    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let _model_server = AbortTask(tokio::spawn(async move {
        axum::serve(model_listener, model_router).await.unwrap();
    }));
    let inference = sail_mind::GemmaRunner::with_scheduler(
        sail_mind::GemmaConfig {
            server_url: format!("http://{model_addr}/v1/chat/completions"),
            model_path: "/nonexistent/fixture-model.gguf".into(),
            ..Default::default()
        },
        Arc::new(sail_common::resources::ResourceScheduler::new(
            Arc::new(FixtureHost),
            Default::default(),
        )),
    )
    .service()
    .unwrap();
    let token = "fixture-live-context-owner-token-32-bytes";
    let socket_state = Arc::new(ServerState {
        bus: EventBus::new(),
        services: services.clone().with_inference(inference.clone()),
        arena: arena.clone(),
        inference,
        plans: None,
        live_context_authority: Some(
            sail_guardian::authority::ApprovalAuthority::new(token).unwrap(),
        ),
        pulse: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/ws/voice", get(ws_handler))
        .with_state(socket_state);
    let server = AbortTask(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let (mut socket, _) = connect_async(format!("ws://{addr}/ws/voice"))
        .await
        .unwrap();
    let hello = receive_type(&mut socket, "session").await;
    assert_eq!(hello["learning"], true);
    let socket_session = hello["session_id"].as_str().unwrap();
    arena.set_session_learning(socket_session, true).unwrap();
    socket
        .send(WsMessage::Text(
            json!({"action":"set_live_context","enabled":true}).to_string(),
        ))
        .await
        .unwrap();
    assert!(
        receive_type(&mut socket, "error").await["message"]
            .as_str()
            .unwrap()
            .contains("owner authentication")
    );
    socket
        .send(WsMessage::Text(
            json!({"action":"set_live_context","enabled":true,"owner_token":token}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(
        receive_type(&mut socket, "live_context_state").await["enabled"],
        true
    );
    socket
        .send(WsMessage::Text(
            json!({"action":"client_speech","stt":true,"tts":true}).to_string(),
        ))
        .await
        .unwrap();
    receive_type(&mut socket, "client_speech").await;
    for command in [
        json!({"action":"speech_start","capture_id":1}),
        json!({"action":"voice_text","capture_id":1,"text":"The launch is on Tuesday."}),
    ] {
        socket
            .send(WsMessage::Text(command.to_string()))
            .await
            .unwrap();
    }
    assert_eq!(
        receive_type(&mut socket, "live_context_observed").await["text"],
        "The launch is on Tuesday."
    );
    receive_type(&mut socket, "turn_finished").await;
    assert!(
        requests.lock().await.is_empty(),
        "ambient speech must never call the model"
    );
    assert!(
        arena
            .conversation_context(socket_session, "launch")
            .hot
            .is_empty()
    );
    socket
        .send(WsMessage::Text(
            json!({"action":"text","text":"What date was agreed?"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(
        receive_type(&mut socket, "response_delta").await["text"],
        "The launch is on Tuesday."
    );
    receive_type(&mut socket, "turn_finished").await;
    let observed_request = requests.lock().await[0].clone();
    let system = observed_request["messages"][0]["content"].as_str().unwrap();
    assert!(!system.contains("The launch is on Tuesday."));
    let user = observed_request["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(user.contains("Live conversation observations"));
    assert!(user.contains("The launch is on Tuesday."));
    assert!(user.contains("untrusted quoted speech"));
    assert!(user.contains(crate::vlm::MEMORY_CONTEXT_BEGIN));
    assert!(
        observed_request["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["content"]
            .to_string()
            .contains("What date was agreed?")
    );

    // Old transcripts must not replace or consume a newer capture.
    for command in [
        json!({"action":"speech_start","capture_id":2}),
        json!({"action":"speech_start","capture_id":3}),
        json!({"action":"voice_text","capture_id":2,"text":"stale private speech"}),
        json!({"action":"voice_text","capture_id":3,"text":"current capture"}),
    ] {
        socket
            .send(WsMessage::Text(command.to_string()))
            .await
            .unwrap();
    }
    assert_eq!(
        receive_type(&mut socket, "live_context_observed").await["text"],
        "current capture"
    );
    receive_type(&mut socket, "turn_finished").await;
    // Reset clears the buffer and invalidates a transcription still on the device.
    socket
        .send(WsMessage::Text(
            json!({"action":"speech_start","capture_id":4}).to_string(),
        ))
        .await
        .unwrap();
    socket
        .send(WsMessage::Text(json!({"action":"reset"}).to_string()))
        .await
        .unwrap();
    assert_eq!(
        receive_type(&mut socket, "live_context_state").await["enabled"],
        false
    );
    // Stopping does not require an available owner token.
    socket
        .send(WsMessage::Text(
            json!({"action":"set_live_context","enabled":false}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(
        receive_type(&mut socket, "live_context_state").await["enabled"],
        false
    );
    socket
        .send(WsMessage::Text(
            json!({"action":"voice_text","capture_id":4,"text":"late after reset"}).to_string(),
        ))
        .await
        .unwrap();
    socket
        .send(WsMessage::Text(
            json!({"action":"text","text":"What context do you have now?"}).to_string(),
        ))
        .await
        .unwrap();
    receive_type(&mut socket, "turn_finished").await;
    let recorded = requests.lock().await;
    assert_eq!(
        recorded.len(),
        2,
        "late speech must never trigger reasoning"
    );
    assert!(
        !recorded[1]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Live conversation observations")
    );
    assert!(!recorded[1].to_string().contains("late after reset"));
    drop(recorded);
    socket.close(None).await.unwrap();
    drop(server);
    unsafe {
        match old_python {
            Some(v) => std::env::set_var("SAIL_VOICE_PYTHON", v),
            None => std::env::remove_var("SAIL_VOICE_PYTHON"),
        };
        match old_worker {
            Some(v) => std::env::set_var("SAIL_VOICE_WORKER", v),
            None => std::env::remove_var("SAIL_VOICE_WORKER"),
        };
    }
    std::fs::remove_file(path).unwrap();
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event.value);
    }
    assert!(events.iter().any(|e| e["type"] == "live_context_observed"));
    assert!(!events.iter().any(|e| matches!(
        e["type"].as_str(),
        Some("turn_started" | "tool_started" | "audio")
    )));
    assert!(
        arena
            .conversation_context("fixture", "launch")
            .hot
            .is_empty(),
        "ambient speech must not enter normal learning/archive"
    );
    assert!(
        context
            .lock()
            .unwrap()
            .prompt_context(Instant::now())
            .unwrap()
            .contains("Tuesday")
    );
    context.lock().unwrap().set_enabled(false);
    assert!(
        context
            .lock()
            .unwrap()
            .prompt_context(Instant::now())
            .is_none()
    );
}
