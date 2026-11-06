//! Serve the real web UI and Memory API over a throwaway, synthetic knowledge store.
//!
//! For UI development and the browser regression check; it never touches SAIL's own
//! memory. Run:
//!
//!     cargo run -p sail-dialogue --example memory_ui_demo -- 127.0.0.1:8791
//!
//! then open http://127.0.0.1:8791/#arena. No model, microphone or voice backend runs, so
//! the rest of the interface reports those services as unavailable.
use axum::{Json, Router, routing::get};
use sail_arena::MemoryStore;
use sail_arena::knowledge::*;
use std::sync::Arc;

const HOUR: i64 = 3_600_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: std::net::SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8791".into())
        .parse()?;
    if !addr.ip().is_loopback() {
        return Err("the demo only listens on loopback addresses".into());
    }
    let token = format!("demo-owner-token-{}", uuid::Uuid::new_v4().simple());
    let dir = std::env::temp_dir().join(format!("sail-memory-demo-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(MemoryStore::open(dir.join("arena.db"))?);
    seed(&store)?;
    let authority = sail_guardian::authority::ApprovalAuthority::new(&token).ok();
    let app = Router::new()
        .nest_service(
            "/api/memory",
            sail_dialogue::memory_http::router(store, authority),
        )
        .route(
            "/api/guardian/token",
            get(move || {
                let token = token.clone();
                async move { Json(serde_json::json!({"token": token, "configured": true})) }
            }),
        )
        .merge(sail_dialogue::server::web_assets());
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("Memory demo listening on http://{addr}/#arena (data in {})", dir.display());
        axum::serve(listener, app).await
    })?;
    Ok(())
}

fn seed(store: &MemoryStore) -> KnowledgeResult<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let t0 = now - 72 * HOUR;
    store.knowledge().write(Actor::subsystem("demo"), |tx| {
        let entity = |tx: &mut KnowledgeTx<'_>, kind, name: &str, aliases: &[&str], scope: &str| {
            tx.create_entity(&NewEntity { kind, name, aliases, scope, sensitivity: Sensitivity::Normal })
        };
        let evidence = |tx: &mut KnowledgeTx<'_>, kind, text: &str, at: i64, scope: &str| {
            tx.add_evidence(&NewEvidence {
                source_kind: kind,
                source_ref: Some("demo"),
                observed_at: at,
                content: text,
                receipt: Some(text),
                derived_from: None,
                verification: None,
                scope,
            })
        };
        let claim = |subject, predicate, object, status, at, scope| NewClaim {
            subject,
            predicate,
            object,
            category: ClaimCategory::Fact,
            basis: None,
            status,
            inference_confidence: None,
            observed_at: at,
            valid_from: None,
            valid_until: None,
            retention: Retention::Durable,
            scope,
            sensitivity: Sensitivity::Normal,
        };
        let v = |s: &str| ClaimObject::Value(s.into());

        let me = entity(tx, EntityKind::Person, "Owner", &["me"], "user")?;
        let sail = entity(tx, EntityKind::Project, "SAIL", &["the assistant project"], "project:sail")?;
        let host = entity(tx, EntityKind::Environment, "SAIL host", &["ASUS desktop"], "system")?;
        let rust = entity(tx, EntityKind::Concept, "Rust", &[], "project:sail")?;
        let disk = entity(tx, EntityKind::Device, "Ubuntu root partition", &[], "system")?;
        entity(tx, EntityKind::Person, "Alex", &[], "user")?;
        entity(tx, EntityKind::Person, "Alex", &[], "user")?;

        let said = evidence(tx, SourceKind::UserStatement, "I build SAIL in Rust and prefer local inference.", t0, "user")?;
        let cargo = evidence(tx, SourceKind::ExternalArtifact, "Cargo.toml: workspace of 14 Rust crates", t0, "project:sail")?;
        tx.assert_claim(&claim(me, "works_on", ClaimObject::Entity(sail), EpistemicStatus::UserReported, t0, "project:sail"), &[(said, Stance::Supports)])?;
        tx.assert_claim(&claim(sail, "primary_language", ClaimObject::Entity(rust), EpistemicStatus::Verified, t0, "project:sail"), &[(cargo, Stance::Supports)])?;
        tx.assert_claim(&claim(sail, "runs_on", ClaimObject::Entity(host), EpistemicStatus::UserReported, t0, "project:sail"), &[(said, Stance::Supports)])?;
        tx.assert_claim(&claim(host, "operating_system", v("Ubuntu Linux"), EpistemicStatus::Verified, t0, "system"), &[(cargo, Stance::Supports)])?;
        tx.assert_claim(&claim(host, "contains", ClaimObject::Entity(disk), EpistemicStatus::Verified, t0, "system"), &[(cargo, Stance::Supports)])?;

        let pref = |value, basis, status, confidence, at| NewClaim {
            category: ClaimCategory::Preference,
            basis: Some(basis),
            inference_confidence: confidence,
            ..claim(me, "prefers", v(value), status, at, "user")
        };
        tx.assert_claim(&pref("local inference", PreferenceBasis::Explicit, EpistemicStatus::UserReported, None, t0), &[(said, Stance::Supports)])?;
        let guess = evidence(tx, SourceKind::ModelInference, "Owner closed the dense dashboard twice.", t0 + HOUR, "user")?;
        tx.assert_claim(&pref("minimal interfaces", PreferenceBasis::Inferred, EpistemicStatus::Inferred, Some(0.6), t0 + HOUR), &[(guess, Stance::Supports)])?;

        // Disk usage observed twice: the newer reading supersedes the older one.
        let full = evidence(tx, SourceKind::ToolObservation, "df -h /: 100% used", t0, "system")?;
        let old = tx.assert_claim(&claim(disk, "usage", v("100%"), EpistemicStatus::Verified, t0, "system"), &[(full, Stance::Supports)])?;
        let freed = evidence(tx, SourceKind::ToolObservation, "df -h /: 58% used", t0 + 24 * HOUR, "system")?;
        let new = tx.assert_claim(&claim(disk, "usage", v("58%"), EpistemicStatus::Verified, t0 + 24 * HOUR, "system"), &[(freed, Stance::Supports)])?;
        tx.supersede(old, new, "newer df reading")?;

        // Two sources disagree about the GPU memory.
        let spec = evidence(tx, SourceKind::UserStatement, "The GPU has 8 GB.", t0, "system")?;
        let lspci = evidence(tx, SourceKind::ToolObservation, "rocm-smi: VRAM total 4096 MB", t0 + 2 * HOUR, "system")?;
        tx.assert_claim(&claim(host, "gpu_memory", v("8 GB"), EpistemicStatus::UserReported, t0, "system"), &[(spec, Stance::Supports)])?;
        tx.assert_claim(&claim(host, "gpu_memory", v("4 GB"), EpistemicStatus::Verified, t0 + 2 * HOUR, "system"), &[(lspci, Stance::Supports)])?;

        tx.assert_claim(
            &NewClaim { category: ClaimCategory::Question, ..claim(sail, "open_question", v("Which embedding model should Arena use?"), EpistemicStatus::Unverified, t0, "project:sail") },
            &[(said, Stance::Supports)],
        )?;

        let task = tx.add_project_item(&NewProjectItem { project: sail, kind: ProjectItemKind::Task, title: "Move Cargo build artifacts to the runtime SSD", detail: Some("Root partition reached 100%."), state: WorkState::Approved, scope: "project:sail", sensitivity: Sensitivity::Normal })?;
        let metadata = evidence(tx, SourceKind::ToolObservation, "cargo metadata: target_directory=/srv/sail-data/target", t0 + 24 * HOUR, "project:sail")?;
        tx.set_project_state(task, WorkState::Implemented, Some(metadata), Some("verified with cargo metadata"))?;
        tx.add_project_item(&NewProjectItem { project: sail, kind: ProjectItemKind::Decision, title: "Keep model weights on the SSD", detail: None, state: WorkState::Approved, scope: "project:sail", sensitivity: Sensitivity::Normal })?;

        let skill = entity(tx, EntityKind::Procedure, "Relocate Cargo build artifacts", &[], "project:sail")?;
        let (procedure, _) = tx.put_procedure(skill, &ProcedureSpec {
            summary: "Move Cargo's target directory to a larger disk safely.".into(),
            preconditions: vec!["Destination filesystem is mounted".into(), "Active builds are stopped".into()],
            required_capabilities: vec!["Write to /srv/sail-data".into()],
            steps: vec![
                ProcedureStep { description: "Configure the Cargo target directory".into(), workflow_ref: Some("forge:cargo_target_dir".into()) },
                ProcedureStep { description: "Build once to validate the configuration".into(), workflow_ref: None },
                ProcedureStep { description: "Remove the old build artifacts after verification".into(), workflow_ref: None },
            ],
            postconditions: vec!["Builds use the new destination".into(), "Root partition usage decreases".into()],
            ..Default::default()
        })?;
        tx.record_procedure_run(procedure, RunOutcome::Succeeded, metadata, None)?;
        Ok(())
    })
}
