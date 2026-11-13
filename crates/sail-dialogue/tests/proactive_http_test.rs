//! Initiative and escalation over HTTP and through the `escalate_reasoning` tool.
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sail_dialogue::proactive::Proactive;
use sail_dialogue::proactive_http;
use sail_discovery::agenda::AgendaStore;
use sail_discovery::detect::{Observation, SchoolItemKind};
use sail_discovery::initiative::Initiative;
use sail_guardian::authority::ApprovalAuthority;
use sail_mind::escalation::provider::{ExternalProvider, ProviderError, ProviderReply};
use sail_mind::escalation::service::{self, EscalationService};
use sail_mind::escalation::store::EscalationStore;
use sail_mind::escalation::{EscalationPolicy, Outcome, ProviderConfig};
use sail_pulse::attention::AttentionContext;
use sail_pulse::attention::store::AttentionStore;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn engine(escalation: Option<Arc<EscalationService>>) -> Arc<Proactive> {
    let initiative = Arc::new(Initiative::new(
        Arc::new(AttentionStore::open_in_memory().unwrap()),
        Arc::new(AgendaStore::open_in_memory().unwrap()),
    ));
    Proactive::new(initiative, escalation, None)
}

async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

fn context() -> AttentionContext {
    AttentionContext {
        now: Utc::now(),
        quiet_hours: false,
        focus_until: None,
        voice_active: false,
        busy: false,
        interrupts_last_hour: 0,
        suggestions_today: 0,
    }
}

#[tokio::test]
async fn inbox_feedback_settings_and_agenda_over_http() {
    let proactive = engine(None);
    let auth = || Some(ApprovalAuthority::new(TOKEN).unwrap());
    let base = serve(proactive_http::initiative_router(proactive.clone(), auth())).await;
    let esc = serve(proactive_http::escalation_router(proactive.clone(), auth())).await;
    let client = reqwest::Client::new();
    let get = |url: String| client.get(url).bearer_auth(TOKEN).send();

    // Owner token required.
    let res = client.get(format!("{base}/inbox")).send().await.unwrap();
    assert_eq!(res.status(), 401);

    let exam = Observation::SchoolItem {
        id: "e1".into(),
        course: "Física".into(),
        kind: SchoolItemKind::Exam,
        title: "Exame".into(),
        due: Utc::now() + Duration::minutes(100),
        weight_percent: Some(50.0),
        confidence: 0.95,
    };
    let report = proactive.initiative.tick(&[exam], context());
    assert_eq!(report.interrupt, 1, "{report:?}");

    let items: Value = get(format!("{base}/deliverable"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = items[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(items[0]["state"], "delivered");
    let again: Value = get(format!("{base}/deliverable"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again, json!([]));

    // Later must be in the future.
    let past = client
        .post(format!("{base}/items/{id}/feedback"))
        .bearer_auth(TOKEN)
        .json(&json!({"feedback":"later","until": Utc::now() - Duration::hours(1)}))
        .send()
        .await
        .unwrap();
    assert_eq!(past.status(), 400);
    let later: Value = client
        .post(format!("{base}/items/{id}/feedback"))
        .bearer_auth(TOKEN)
        .json(&json!({"feedback":"later","until": Utc::now() + Duration::hours(1)}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(later["state"], "snoozed");
    let missing = client
        .post(format!("{base}/items/nope/feedback"))
        .bearer_auth(TOKEN)
        .json(&json!({"feedback":"useful"}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    // Settings round-trip and validation.
    let mut settings: Value = get(format!("{base}/settings"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settings["autonomy"], "balanced");
    settings["autonomy"] = json!("quiet");
    let saved = client
        .post(format!("{base}/settings"))
        .bearer_auth(TOKEN)
        .json(&settings)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let reread: Value = get(format!("{base}/settings"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reread["autonomy"], "quiet");
    settings["interrupt_threshold"] = json!(4.0);
    let bad = client
        .post(format!("{base}/settings"))
        .bearer_auth(TOKEN)
        .json(&settings)
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    let investigate: Value = client
        .post(format!("{base}/items/{id}/feedback"))
        .bearer_auth(TOKEN)
        .json(&json!({"feedback":"investigate"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(investigate["state"], "investigating");

    let agenda: Value = get(format!("{base}/agenda"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kinds: Vec<&str> = agenda
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"commitment"), "{agenda}");
    assert!(kinds.contains(&"investigation"), "{agenda}");
    let stats: Value = get(format!("{base}/stats"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(stats["attention"]["by_delivery"].is_array());

    // No escalation service → 503, not a fake status.
    let status = get(format!("{esc}/status")).await.unwrap();
    assert_eq!(status.status(), 503);
}

struct Fake;
#[async_trait]
impl ExternalProvider for Fake {
    fn id(&self) -> &str {
        "fake"
    }
    fn model(&self) -> &str {
        "fake-large"
    }
    async fn complete(
        &self,
        _system: &str,
        prompt: &str,
        _max_tokens: u32,
        _cancel: CancellationToken,
    ) -> Result<ProviderReply, ProviderError> {
        // The provider must only ever see placeholders.
        assert!(!prompt.contains("ana@example.com"), "{prompt}");
        Ok(ProviderReply {
            text: format!("Write to [EMAIL_1]. Seen: {}", prompt.len()),
            model: "fake-large".into(),
            input_tokens: 120,
            output_tokens: 40,
        })
    }
}

#[tokio::test]
async fn escalate_reasoning_tool_redacts_answers_caches_and_reports() {
    let policy = EscalationPolicy {
        enabled: true,
        providers: vec![ProviderConfig {
            id: "fake".into(),
            adapter: "openai_compatible".into(),
            base_url: "http://127.0.0.1:9".into(),
            model: "fake-large".into(),
            api_key_env: "SAIL_TEST_UNSET_KEY".into(),
            kinds: vec![],
            input_price_micros_per_mtok: 1_000_000,
            output_price_micros_per_mtok: 2_000_000,
            enabled: true,
            command: None,
            cli_unverified_ok: false,
        }],
        route_threshold: 0.0,
        ..EscalationPolicy::default()
    };
    let service =
        EscalationService::new(policy, Arc::new(EscalationStore::open_in_memory().unwrap()));
    service.insert_provider(Arc::new(Fake));
    assert!(service::install_global(service.clone()));

    // Offered to the model now that the owner enabled it.
    let offered = sail_dialogue::vlm::tool_definitions();
    assert!(
        offered
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "escalate_reasoning")
    );

    let input = json!({
        "kind": "reasoning",
        "question": "Should I e-mail ana@example.com about the schedule clash?",
        "justification": "Needs careful trade-off reasoning",
        "expected_benefit": "A clearer recommendation",
        "token_budget": 500,
        "local_attempts": 1,
        "complexity": 0.8
    });
    let first = sail_dialogue::execute_escalation(&input, None)
        .await
        .unwrap();
    assert_eq!(first["decision"]["decision"], "escalate", "{first}");
    assert_eq!(first["answering_model"], "fake-large");
    // Re-hydrated locally.
    assert!(
        first["answer"]
            .as_str()
            .unwrap()
            .contains("ana@example.com"),
        "{first}"
    );

    let second = sail_dialogue::execute_escalation(&input, None)
        .await
        .unwrap();
    assert_eq!(second["decision"]["decision"], "cached", "{second}");

    let proactive = engine(Some(service.clone()));
    let esc = serve(proactive_http::escalation_router(
        proactive,
        Some(ApprovalAuthority::new(TOKEN).unwrap()),
    ))
    .await;
    let client = reqwest::Client::new();
    let status: Value = client
        .get(format!("{esc}/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["usage"]["tokens_today"], 160);
    assert_eq!(status["usage"]["calls_today"], 1);
    assert_eq!(status["recent_decisions"].as_array().unwrap().len(), 2);
    let job = first["job_id"].as_str().unwrap();
    let ok = client
        .post(format!("{esc}/jobs/{job}/outcome"))
        .bearer_auth(TOKEN)
        .json(&json!({"outcome":"improved"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let status: Value = client
        .get(format!("{esc}/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["outcomes"]["by_kind"][0][1], 1);

    // Tied to a Forge job: filed under `forge:<id>`, so the job's passing tests can
    // record that the escalation helped (#450).
    let forge_input = json!({
        "kind": "coding",
        "question": "Why does the borrow checker reject this closure capturing a guard?",
        "justification": "Two local attempts failed to compile",
        "expected_benefit": "The Forge job can finish",
        "token_budget": 500,
        "local_attempts": 2,
        "complexity": 0.9,
        "forge_job_id": "job-7"
    });
    let forged = sail_dialogue::execute_escalation(&forge_input, Some("turn-1".into()))
        .await
        .unwrap();
    let forged_job = forged["job_id"].as_str().unwrap().to_owned();
    assert_eq!(
        service
            .observe_correlated("forge:job-7", Outcome::Improved, "forge_tests")
            .unwrap(),
        1
    );
    let job = service.store().job(&forged_job).unwrap().unwrap();
    assert_eq!(job.correlation_id.as_deref(), Some("forge:job-7"));
    assert_eq!(job.outcome, Some(Outcome::Improved));
    // The owner's word beats the check.
    service.feedback(&forged_job, Outcome::Worse).unwrap();
    assert!(
        !service
            .observe_outcome(&forged_job, Outcome::Improved, "forge_tests")
            .unwrap()
    );
}

struct NotesInvestigator;

#[async_trait::async_trait]
impl sail_discovery::curiosity::Investigator for NotesInvestigator {
    async fn investigate(
        &self,
        brief: &sail_discovery::curiosity::Brief,
        _limits: &sail_discovery::curiosity::CuriosityLimits,
        _cancel: CancellationToken,
    ) -> Result<sail_discovery::curiosity::Finding, String> {
        Ok(sail_discovery::curiosity::Finding {
            summary: format!("Notes on {}: the course page lists it at 30%.", brief.title),
            sources: vec!["moodle: avaliação".into()],
            tokens_used: 300,
            tool_steps: 1,
            escalation_job: None,
            conclusive: true,
        })
    }
}

#[tokio::test]
async fn curiosity_respects_autonomy_and_reports_receipts() {
    use sail_discovery::agenda::{AgendaKind, NewAgendaItem};
    use sail_discovery::curiosity::{CuriosityLedger, Skip};
    use sail_pulse::attention::{Autonomy, Provenance};

    let proactive = engine(None);
    assert!(proactive.enable_curiosity(
        Arc::new(NotesInvestigator),
        CuriosityLedger::open_in_memory().unwrap()
    ));
    proactive
        .initiative
        .agenda()
        .upsert(
            NewAgendaItem {
                key: "investigate:fisica".into(),
                kind: AgendaKind::Investigation,
                title: "Física project weight".into(),
                detail: String::new(),
                provenance: Provenance::UserCommitment,
                match_terms: vec![],
                due: None,
            },
            Utc::now(),
        )
        .unwrap();
    let attention = proactive.initiative.attention();
    let mut settings = attention.settings().unwrap();
    settings.autonomy = Autonomy::Quiet;
    attention.save_settings(&settings).unwrap();
    assert_eq!(proactive.maybe_investigate(), Err(Skip::Autonomy));

    settings.autonomy = Autonomy::Balanced;
    attention.save_settings(&settings).unwrap();
    assert_eq!(proactive.maybe_investigate(), Ok(()));
    let curiosity = proactive.curiosity().unwrap();
    for _ in 0..100 {
        if !curiosity.running() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let base = serve(proactive_http::initiative_router(
        proactive.clone(),
        Some(ApprovalAuthority::new(TOKEN).unwrap()),
    ))
    .await;
    let state: Value = reqwest::Client::new()
        .get(format!("{base}/curiosity"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["enabled"], true, "{state}");
    assert_eq!(state["runs_today"], 1, "{state}");
    let receipt = &state["receipts"][0];
    assert_eq!(receipt["outcome"], "finding", "{state}");
    assert_eq!(receipt["tokens_used"], 300);
    assert_eq!(receipt["item_title"], "Física project weight");

    // The finding reaches the inbox on the next tick, as SAIL's hypothesis.
    proactive.tick_now();
    let items = attention.inbox(50).unwrap();
    let finding = items
        .iter()
        .find(|i| i.signal.source == sail_pulse::attention::Source::Curiosity)
        .expect("finding ingested");
    assert_eq!(finding.signal.provenance, Provenance::Hypothesis);
    assert!(finding.signal.body.starts_with("SAIL's unverified notes:"));
}

#[tokio::test]
async fn escalation_status_exposes_every_field_read_by_usage_view() {
    let service = EscalationService::new(
        EscalationPolicy::default(),
        Arc::new(EscalationStore::open_in_memory().unwrap()),
    );
    let proactive = engine(Some(service));
    let esc = serve(proactive_http::escalation_router(
        proactive,
        Some(ApprovalAuthority::new(TOKEN).unwrap()),
    ))
    .await;
    let status: Value = reqwest::Client::new()
        .get(format!("{esc}/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(status.get("enabled").is_some_and(Value::is_boolean));
    assert!(status.get("providers").is_some_and(Value::is_array));
    assert!(status.get("daily_token_cap").is_some_and(Value::is_u64));
    assert!(status.get("monthly_token_cap").is_some_and(Value::is_u64));
    assert!(
        status
            .get("daily_cost_cap_micros")
            .is_some_and(Value::is_u64)
    );
    assert!(
        status
            .get("monthly_cost_cap_micros")
            .is_some_and(Value::is_u64)
    );

    let usage = &status["usage"];
    assert!(usage.get("tokens_today").is_some_and(Value::is_u64));
    assert!(usage.get("tokens_month").is_some_and(Value::is_u64));
    assert!(usage.get("cost_today_micros").is_some_and(Value::is_u64));
    assert!(usage.get("cost_month_micros").is_some_and(Value::is_u64));
    assert!(usage.get("calls_today").is_some_and(Value::is_u64));
    assert!(usage.get("blocked_until").is_some_and(Value::is_array));

    assert!(
        status["outcomes"]
            .get("by_kind")
            .is_some_and(Value::is_array)
    );
    assert!(status.get("recent_decisions").is_some_and(Value::is_array));
}
