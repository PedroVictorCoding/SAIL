//! K0 contract test: every Night Shift client fixture deserializes into the Rust types.
//!
//! Antigravity writes the fixtures from the HTTP contract. Until they are logged, the
//! directory is absent and this test skips with a comment (K0 must not depend on G0).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sail_forge::nightshift::{
    Deferred, Errand, ErrandId, ErrandSource, ErrandState, MorningReport, NightItem, NightPlan,
    NightShiftSettings, RanOn, WorkOrder,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct GetBody {
    settings: NightShiftSettings,
    tonight: Tonight,
    #[serde(default)]
    errands: Vec<Errand>,
    morning: Option<MorningReport>,
    mesh: Option<Mesh>,
    external: External,
}

#[derive(Debug, Deserialize)]
struct Tonight {
    window: String,
    plan: NightPlan,
}

#[derive(Debug, Deserialize)]
struct Mesh {
    available: bool,
    name: String,
    resting: bool,
}

#[derive(Debug, Deserialize)]
struct External {
    provider: Option<String>,
    used_tonight: u32,
    allowed: u32,
}

#[derive(Debug, Deserialize)]
struct WsErrand {
    #[serde(rename = "type")]
    kind: String,
    errand: Errand,
}

#[derive(Debug, Deserialize)]
struct WsMorning {
    #[serde(rename = "type")]
    kind: String,
    report: MorningReport,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ui/fixtures/nightshift")
}

fn fixture_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("fixture directory is readable")
        .map(|entry| entry.expect("fixture entry is readable").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

fn assert_valid_errand(errand: &Errand, source: &str) {
    assert_eq!(
        ErrandId::parse(errand.id.as_str()).as_ref(),
        Some(&errand.id),
        "{source}: errand id must be e- plus 12 lowercase hex"
    );
    assert!(!errand.ask.is_empty(), "{source}: ask is empty");
    assert!(
        errand.ask.chars().count() <= 2_000,
        "{source}: ask too long"
    );
    assert!(
        errand.updated_at >= errand.created_at,
        "{source}: time went backwards"
    );
    match &errand.state {
        ErrandState::Proposed { order } | ErrandState::Queued { order, .. } => {
            assert_valid_order(order, source)
        }
        ErrandState::Working { order, stage, .. } => {
            assert_valid_order(order, source);
            assert!(
                matches!(
                    stage.as_str(),
                    "planning" | "coding" | "testing" | "reviewing"
                ),
                "{source}: unknown stage {stage:?}"
            );
        }
        ErrandState::Ready { order, .. }
        | ErrandState::NeedsYou { order, .. }
        | ErrandState::Failed { order, .. }
        | ErrandState::Shipping { order, .. }
        | ErrandState::Shipped { order, .. } => assert_valid_order(order, source),
        ErrandState::Unshapeable { reasons } => assert_valid_reasons(reasons, source),
        ErrandState::Shaping | ErrandState::Dropped { .. } => {}
    }
}

fn assert_valid_order(order: &WorkOrder, source: &str) {
    assert!(!order.title.is_empty(), "{source}: title is empty");
    assert!(
        !order.acceptance.is_empty(),
        "{source}: no acceptance checks"
    );
    assert!(!order.target_files.is_empty(), "{source}: no target files");
}

fn assert_valid_item(item: &NightItem, source: &str) {
    match item {
        NightItem::Errand { id } => {
            assert!(
                ErrandId::parse(id.as_str()).is_some(),
                "{source}: plan item has an invalid errand id"
            );
        }
        NightItem::Issue { number } => {
            assert!(*number > 0, "{source}: plan issue number is zero");
        }
    }
}

fn assert_valid_plan(plan: &NightPlan, source: &str) {
    for item in &plan.items {
        assert_valid_item(item, source);
    }
    for Deferred { item, reason } in &plan.deferred {
        assert_valid_item(item, source);
        assert!(
            !reason.trim().is_empty(),
            "{source}: deferred reason is empty"
        );
    }
}

fn assert_valid_reasons(reasons: &[String], source: &str) {
    assert!(!reasons.is_empty(), "{source}: unshapeable has no reasons");
    for reason in reasons {
        assert!(!reason.trim().is_empty(), "{source}: empty reason");
    }
}

fn assert_valid_morning(report: &MorningReport, source: &str) {
    assert_eq!(report.date.len(), 10, "{source}: date is not YYYY-MM-DD");
    assert_eq!(
        report.ready + report.needs_you + report.failed + report.not_reached,
        report.items.len() as u32,
        "{source}: morning counts do not match the items"
    );
    for item in &report.items {
        assert_valid_item(&item.item, source);
        assert!(
            !item.title.is_empty(),
            "{source}: morning item has no title"
        );
    }
    assert!(
        report
            .spoken
            .split(['.', '!', '?'])
            .filter(|s| !s.trim().is_empty())
            .count()
            <= 2,
        "{source}: spoken morning report exceeds two sentences"
    );
}

#[test]
fn every_nightshift_client_fixture_matches_the_rust_contracts() {
    let dir = fixture_dir();
    if !dir.is_dir() {
        // G0's fixtures are not logged yet; the contract test runs once they exist.
        eprintln!(
            "skipping Night Shift fixtures: {} does not exist yet",
            dir.display()
        );
        return;
    }
    let files = fixture_files(&dir);
    assert!(
        !files.is_empty(),
        "no Night Shift fixtures in {}",
        dir.display()
    );

    let mut checked = 0;
    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("fixture")
            .to_string();
        let text = std::fs::read_to_string(&path).expect("fixture is readable");
        match name.as_str() {
            "ws_errand.json" => {
                let ws: WsErrand =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(ws.kind, "nightshift", "{name}: wrong ws type");
                assert_valid_errand(&ws.errand, &name);
            }
            "ws_morning.json" => {
                let ws: WsMorning =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(ws.kind, "nightshift_morning", "{name}: wrong ws type");
                assert_valid_morning(&ws.report, &name);
            }
            _ => {
                let body: GetBody =
                    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(
                    body.settings.window_start_hour < 24 && body.settings.window_end_hour < 24,
                    "{name}: window hours out of range"
                );
                assert!(
                    !body.tonight.window.is_empty(),
                    "{name}: empty window label"
                );
                assert_valid_plan(&body.tonight.plan, &name);
                for errand in &body.errands {
                    assert_valid_errand(errand, &name);
                }
                if let Some(morning) = &body.morning {
                    assert_valid_morning(morning, &name);
                }
                if let Some(mesh) = &body.mesh {
                    assert!(!mesh.name.is_empty(), "{name}: mesh has no name");
                    let _ = (mesh.available, mesh.resting);
                }
                assert!(
                    body.external.used_tonight <= 3 && body.external.allowed <= 3,
                    "{name}: external help exceeds the per-night bound"
                );
                if let Some(provider) = &body.external.provider {
                    assert!(
                        matches!(provider.as_str(), "claude_cli" | "codex_cli"),
                        "{name}: unknown external provider {provider:?}"
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "no GET fixtures in {}", dir.display());
}

#[test]
fn errand_id_is_strictly_e_plus_twelve_lowercase_hex() {
    assert!(ErrandId::parse("e-0a1b2c3d4e5f").is_some());
    assert!(ErrandId::parse("e-000000000000").is_some());
    assert!(ErrandId::parse("e-0A1B2C3D4E5F").is_none());
    assert!(ErrandId::parse("e-0a1b2c3d4e5").is_none());
    assert!(ErrandId::parse("e-0a1b2c3d4e5ff").is_none());
    assert!(ErrandId::parse("x-0a1b2c3d4e5f").is_none());
    assert!(ErrandId::parse("e-0a1b2c3d4e5g").is_none());
    assert!(ErrandId::parse("").is_none());

    let id = ErrandId::new([0x0a, 0x1b, 0x2c, 0x3d, 0x4e, 0x5f]);
    assert_eq!(id.as_str(), "e-0a1b2c3d4e5f");
    assert_eq!(ErrandId::new([0, 0, 0, 0, 0, 0]).as_str(), "e-000000000000");
    assert_eq!(ErrandId::new([0xff; 6]).as_str(), "e-ffffffffffff");
}

#[test]
fn work_order_body_satisfies_existing_admission_parsing() {
    use sail_forge::admission::{AdmissionDecision, AdmissionPolicy, IssueCandidate};
    use sail_forge::parse_target_files;

    let order = WorkOrder {
        title: "Show the next dividend date".into(),
        why: "The owner wants it on the Capital Today tab.".into(),
        acceptance: vec!["The card shows the next dividend date".into()],
        target_files: vec!["crates/sail-dialogue/web/capital-core.js".into()],
        size: sail_forge::nightshift::Size::S,
        notes_for_solver: "Keep it small.".into(),
    };
    let at: DateTime<Utc> = "2026-11-18T21:15:00Z".parse().expect("fixed test time");
    let notes = vec![sail_forge::nightshift::OwnerNote {
        at,
        text: "Please keep the label short.".into(),
    }];
    let body = order.to_issue_body(&notes);
    assert!(body.contains("## Acceptance"));
    assert!(body.contains("- [ ] The card shows the next dividend date"));
    assert!(body.contains("## Target files"));
    assert!(body.contains("## Notes"));
    assert!(body.contains("## Owner feedback"));
    assert_eq!(
        parse_target_files(&body),
        vec!["crates/sail-dialogue/web/capital-core.js".to_string()]
    );

    let issue = IssueCandidate {
        number: 1,
        title: order.title.clone(),
        body,
        labels: vec!["forge-ready".into()],
        has_open_pr: false,
    };
    assert!(matches!(
        AdmissionPolicy::default().evaluate(&issue),
        AdmissionDecision::Ready { .. }
    ));
}

#[test]
fn settings_defaults_match_the_contract() {
    let defaults = NightShiftSettings::default();
    assert!(!defaults.enabled);
    assert_eq!(defaults.window_start_hour, 1);
    assert_eq!(defaults.window_end_hour, 7);
    assert_eq!(defaults.max_items, 5);
    assert!(defaults.include_forge_ready_issues);
    assert!(defaults.use_mesh);
    assert_eq!(defaults.external_solves_per_night, 0);
    assert_eq!(defaults.external_provider, None);
    assert!(defaults.morning_greeting);

    let from_empty: NightShiftSettings = serde_json::from_str("{}").expect("empty is valid");
    assert_eq!(from_empty, defaults);
}

#[test]
fn unknown_future_fields_and_sources_round_trip() {
    let json = r#"{
        "id":"e-0a1b2c3d4e5f",
        "ask":"Make the tab show dividends",
        "source":"telegram",
        "created_at":"2026-11-18T20:00:00Z",
        "updated_at":"2026-11-18T20:00:00Z",
        "attempts":0,
        "external_used":false,
        "notes":[],
        "state":{"state":"dropped","at":"2026-11-18T20:01:00Z"}
    }"#;
    let errand: Errand = serde_json::from_str(json).expect("errand shape is valid");
    assert_eq!(errand.source, ErrandSource::Telegram);
    let ran_on: RanOn =
        serde_json::from_str(r#"{"kind":"external","name":"Claude Code","model":"claude-sonnet"}"#)
            .expect("ran_on shape is valid");
    assert_eq!(ran_on.kind, sail_forge::nightshift::RanOnKind::External);
}
