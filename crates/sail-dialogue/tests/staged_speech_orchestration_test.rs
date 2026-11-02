use sail_dialogue::{KernelToolResult, SpeechChunker, StagedAckCatalog, TurnCoordinator};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

#[test]
fn staged_ack_catalog_detects_locale_accurately() {
    // English defaults and timbre
    assert_eq!(
        StagedAckCatalog::detect_locale("What is the time?", Some("af_heart")),
        "en"
    );
    assert_eq!(
        StagedAckCatalog::detect_locale("What is the time?", None),
        "en"
    );

    // Portuguese timbre
    assert_eq!(
        StagedAckCatalog::detect_locale("Hello there", Some("pf_dora")),
        "pt"
    );
    assert_eq!(
        StagedAckCatalog::detect_locale("Hello there", Some("pm_alex")),
        "pt"
    );

    // Portuguese prompt markers
    assert_eq!(
        StagedAckCatalog::detect_locale("Qual é a previsão do tempo para Lisboa?", None),
        "pt"
    );
    assert_eq!(
        StagedAckCatalog::detect_locale("Olá SAIL, podes verificar o sistema?", None),
        "pt"
    );
    assert_eq!(
        StagedAckCatalog::detect_locale("Pesquisar tarefas no forge", None),
        "pt"
    );

    // Spanish timbre
    assert_eq!(
        StagedAckCatalog::detect_locale("Hola", Some("ef_dora")),
        "es"
    );
    assert_eq!(
        StagedAckCatalog::detect_locale("Hola SAIL, cómo está el tiempo?", None),
        "es"
    );
}

#[test]
fn staged_ack_catalog_returns_curated_phrases_for_tool_families() {
    let dummy_params = json!({});

    // English
    assert_eq!(
        StagedAckCatalog::phrase_for("data:weather", &dummy_params, "en"),
        "Checking the weather forecast..."
    );
    assert_eq!(
        StagedAckCatalog::phrase_for("web:search", &dummy_params, "en"),
        "Searching the web for that..."
    );
    assert_eq!(
        StagedAckCatalog::phrase_for("os:status", &dummy_params, "en"),
        "Checking system status..."
    );
    assert_eq!(
        StagedAckCatalog::phrase_for("forge:list", &dummy_params, "en"),
        "Checking Forge coding tasks..."
    );
    assert_eq!(
        StagedAckCatalog::phrase_for("fs:read", &dummy_params, "en"),
        "Reading the requested file..."
    );

    // Portuguese
    assert_eq!(
        StagedAckCatalog::phrase_for("data:weather", &dummy_params, "pt"),
        "A verificar a previsão do tempo..."
    );
    assert_eq!(
        StagedAckCatalog::phrase_for("web:research", &dummy_params, "pt"),
        "A pesquisar na web..."
    );
    assert_eq!(
        StagedAckCatalog::phrase_for("os:status", &dummy_params, "pt"),
        "A verificar o estado do sistema..."
    );
    assert_eq!(
        StagedAckCatalog::phrase_for("forge:status", &dummy_params, "pt"),
        "A verificar as tarefas do Forge..."
    );
}

#[tokio::test]
async fn fast_path_skips_ack_when_tool_completes_under_threshold() {
    let events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let ev_clone = events.clone();

    let emit_fn = move |val: serde_json::Value| {
        let ev = ev_clone.clone();
        Box::pin(async move {
            ev.lock().unwrap().push(val);
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    };

    let (speech_tx, mut speech_rx) = mpsc::channel::<String>(16);
    let mut chunker = SpeechChunker::default();
    let mut reply = String::new();

    // Fast tool: completes in 10ms (< 250ms threshold)
    let fast_tool = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        KernelToolResult {
            tool: "data:weather".into(),
            success: true,
            output: json!({"temperature": 21, "condition": "sunny"}),
        }
    };

    let outcome = TurnCoordinator::execute_staged(
        "data:weather",
        &json!({"city": "Lisbon"}),
        "pt",
        fast_tool,
        1,
        &speech_tx,
        &mut chunker,
        &mut reply,
        emit_fn,
        None,
    )
    .await;

    // Fast path: no acknowledgement spoken, no filler in reply!
    assert!(!outcome.ack_spoken);
    assert!(outcome.result.success);
    assert!(reply.is_empty());
    assert!(speech_rx.try_recv().is_err());

    let recorded = events.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0]["type"], "tool_planned");
    assert_eq!(recorded[1]["type"], "tool_completed");
}

#[tokio::test]
async fn slow_tool_emits_ack_and_queues_speech_concurrently() {
    let events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let ev_clone = events.clone();

    let emit_fn = move |val: serde_json::Value| {
        let ev = ev_clone.clone();
        Box::pin(async move {
            ev.lock().unwrap().push(val);
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    };

    let (speech_tx, mut speech_rx) = mpsc::channel::<String>(16);
    let mut chunker = SpeechChunker::default();
    let mut reply = String::new();

    // Slow tool: takes 350ms (> 250ms threshold)
    let slow_tool = async {
        tokio::time::sleep(Duration::from_millis(350)).await;
        KernelToolResult {
            tool: "web:research".into(),
            success: true,
            output: json!({"title": "Rust Concurrency", "url": "https://example.com"}),
        }
    };

    let outcome = TurnCoordinator::execute_staged(
        "web:research",
        &json!({"query": "rust concurrency"}),
        "en",
        slow_tool,
        42,
        &speech_tx,
        &mut chunker,
        &mut reply,
        emit_fn,
        None,
    )
    .await;

    // Acknowledgement was spoken!
    assert!(outcome.ack_spoken);
    assert!(outcome.result.success);
    assert!(outcome.duration_ms >= 300);

    // Spoken acknowledgement was queued to TTS channel
    let queued_text = speech_rx.try_recv().expect("speech chunk queued");
    assert!(queued_text.contains("Searching the web"));
    assert!(reply.contains("Searching the web"));

    // Verify event ordering: tool_planned -> ack_started -> tool_started -> tool_completed
    let recorded = events.lock().unwrap().clone();
    assert_eq!(recorded.len(), 4);
    assert_eq!(recorded[0]["type"], "tool_planned");
    assert_eq!(recorded[1]["type"], "ack_started");
    assert_eq!(recorded[1]["text"], "Searching the web for that...");
    assert_eq!(recorded[2]["type"], "tool_started");
    assert_eq!(recorded[3]["type"], "tool_completed");
    assert_eq!(recorded[3]["duration_ms"], outcome.duration_ms);
}
