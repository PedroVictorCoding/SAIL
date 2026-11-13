//! Curiosity Engine (#453): gates, picking, receipts, and findings as digest hypotheses.
use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use sail_discovery::agenda::{AgendaKind, AgendaStatus, AgendaStore, NewAgendaItem};
use sail_discovery::curiosity::{
    Brief, CuriosityLedger, CuriosityLimits, Finding, Gate, Investigator, RunOutcome, Skip, admit,
    pick, run_once,
};
use sail_discovery::detect::detect;
use sail_pulse::attention::store::AttentionStore;
use sail_pulse::attention::{AttentionContext, Autonomy, Delivery, Provenance, Source};
use tokio_util::sync::CancellationToken;

fn gate() -> Gate {
    Gate {
        autonomy: Autonomy::Balanced,
        voice_active: false,
        quiet_hours: false,
        focus: false,
        idle_for: Some(Duration::hours(1)),
        runs_today: 0,
        running: false,
    }
}

#[test]
fn admission_respects_autonomy_voice_quiet_focus_idle_and_the_daily_limit() {
    let limits = CuriosityLimits::default();
    assert_eq!(admit(&gate(), &limits), Ok(()));
    let cases: Vec<(Gate, Skip)> = vec![
        (
            Gate {
                autonomy: Autonomy::Off,
                ..gate()
            },
            Skip::Autonomy,
        ),
        (
            Gate {
                autonomy: Autonomy::Quiet,
                ..gate()
            },
            Skip::Autonomy,
        ),
        (
            Gate {
                voice_active: true,
                ..gate()
            },
            Skip::Voice,
        ),
        (
            Gate {
                quiet_hours: true,
                ..gate()
            },
            Skip::QuietHours,
        ),
        (
            Gate {
                focus: true,
                ..gate()
            },
            Skip::Focus,
        ),
        (
            Gate {
                idle_for: Some(Duration::minutes(5)),
                ..gate()
            },
            Skip::NotIdle,
        ),
        (
            Gate {
                runs_today: 3,
                ..gate()
            },
            Skip::DailyLimit,
        ),
        (
            Gate {
                running: true,
                ..gate()
            },
            Skip::Running,
        ),
    ];
    for (gate, skip) in cases {
        assert_eq!(admit(&gate, &limits), Err(skip), "{gate:?}");
    }
    // No conversation yet counts as idle.
    assert_eq!(
        admit(
            &Gate {
                idle_for: None,
                ..gate()
            },
            &limits
        ),
        Ok(())
    );
}

fn agenda_with(items: &[(&str, AgendaKind)]) -> AgendaStore {
    let agenda = AgendaStore::open_in_memory().unwrap();
    for (title, kind) in items {
        agenda
            .upsert(
                NewAgendaItem {
                    key: format!("k:{title}"),
                    kind: *kind,
                    title: (*title).into(),
                    detail: String::new(),
                    provenance: Provenance::UserCommitment,
                    match_terms: vec![],
                    due: None,
                },
                Utc::now() - Duration::days(1),
            )
            .unwrap();
    }
    agenda
}

#[test]
fn balanced_picks_only_investigations_proactive_also_questions() {
    let agenda = agenda_with(&[
        ("Is the Física project worth 30%?", AgendaKind::OpenQuestion),
        ("Exam on Friday", AgendaKind::Commitment),
    ]);
    let items = agenda.open_items(10).unwrap();
    let limits = CuriosityLimits::default();
    let now = Utc::now();
    assert!(pick(&items, &HashMap::new(), Autonomy::Balanced, &limits, now).is_none());
    let picked = pick(&items, &HashMap::new(), Autonomy::Proactive, &limits, now).unwrap();
    assert_eq!(picked.kind, AgendaKind::OpenQuestion);

    let agenda = agenda_with(&[
        ("A question", AgendaKind::OpenQuestion),
        ("Tokio cancellation safety", AgendaKind::Investigation),
    ]);
    let items = agenda.open_items(10).unwrap();
    // Investigations come first.
    let picked = pick(&items, &HashMap::new(), Autonomy::Proactive, &limits, now).unwrap();
    assert_eq!(picked.kind, AgendaKind::Investigation);
    // Within the cooldown it is skipped; after it, eligible again.
    let mut runs = HashMap::new();
    runs.insert(picked.id.clone(), now - Duration::hours(2));
    let next = pick(&items, &runs, Autonomy::Proactive, &limits, now).unwrap();
    assert_eq!(next.kind, AgendaKind::OpenQuestion);
    runs.insert(picked.id.clone(), now - Duration::hours(25));
    runs.insert(next.id.clone(), now - Duration::hours(26));
    // Both eligible: investigation still first.
    assert_eq!(
        pick(&items, &runs, Autonomy::Proactive, &limits, now)
            .unwrap()
            .id,
        picked.id
    );
}

struct Fake {
    result: Mutex<Option<Result<Finding, String>>>,
    seen: Mutex<Vec<Brief>>,
    hang: bool,
}

impl Fake {
    fn answering(result: Result<Finding, String>) -> Self {
        Self {
            result: Mutex::new(Some(result)),
            seen: Mutex::new(Vec::new()),
            hang: false,
        }
    }
}

#[async_trait]
impl Investigator for Fake {
    async fn investigate(
        &self,
        brief: &Brief,
        _limits: &CuriosityLimits,
        cancel: CancellationToken,
    ) -> Result<Finding, String> {
        self.seen.lock().unwrap().push(brief.clone());
        if self.hang {
            cancel.cancelled().await;
            return Err("cancelled".into());
        }
        self.result.lock().unwrap().take().unwrap()
    }
}

fn finding() -> Finding {
    Finding {
        summary: "The Moodle course page lists the project at 30% and the exam at 70%.".into(),
        sources: vec!["moodle: Física — avaliação".into()],
        tokens_used: 640,
        tool_steps: 2,
        escalation_job: None,
        conclusive: true,
    }
}

#[tokio::test]
async fn a_run_records_a_receipt_and_a_finding_becomes_a_digest_hypothesis() {
    let agenda = agenda_with(&[("Física project weight", AgendaKind::Investigation)]);
    let ledger = CuriosityLedger::open_in_memory().unwrap();
    let fake = Fake::answering(Ok(finding()));
    let limits = CuriosityLimits::default();
    let report = run_once(
        &agenda,
        &ledger,
        &fake,
        Autonomy::Balanced,
        &limits,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(fake.seen.lock().unwrap()[0].title, "Física project weight");
    assert_eq!(report.receipt.outcome, RunOutcome::Finding);
    assert_eq!(report.receipt.tokens_used, 640);
    assert_eq!(report.receipt.tool_steps, 2);
    assert_eq!(ledger.runs_today(Utc::now()).unwrap(), 1);
    assert_eq!(ledger.recent(5).unwrap(), vec![report.receipt.clone()]);
    let item = agenda.get(&report.receipt.item_id).unwrap();
    assert_eq!(item.status, AgendaStatus::InProgress);

    let signals = detect(&report.observation.unwrap(), Utc::now());
    assert_eq!(signals.len(), 1);
    let signal = &signals[0];
    assert_eq!(signal.source, Source::Curiosity);
    assert_eq!(signal.provenance, Provenance::Hypothesis);
    assert!(signal.body.starts_with("SAIL's unverified notes:"));
    assert!(signal.body.contains("moodle: Física"));

    let attention = AttentionStore::open_in_memory().unwrap();
    let delivered = attention
        .ingest(
            signal.clone(),
            AttentionContext {
                now: Utc::now(),
                quiet_hours: false,
                focus_until: None,
                voice_active: false,
                busy: false,
                interrupts_last_hour: 0,
                suggestions_today: 0,
            },
        )
        .unwrap();
    assert!(
        matches!(
            delivered.assessment.delivery,
            Delivery::Digest | Delivery::Silent
        ),
        "{:?}",
        delivered.assessment
    );

    // The same item is not looked into again today.
    let again = Fake::answering(Ok(finding()));
    let skipped = run_once(
        &agenda,
        &ledger,
        &again,
        Autonomy::Balanced,
        &limits,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(skipped.unwrap_err(), Skip::NothingToDo);
}

#[tokio::test]
async fn failures_and_empty_findings_still_leave_receipts_but_no_signal() {
    let limits = CuriosityLimits::default();
    for (result, expected) in [
        (Err("model unavailable".to_string()), RunOutcome::Failed),
        (
            Ok(Finding {
                conclusive: false,
                ..finding()
            }),
            RunOutcome::NothingFound,
        ),
    ] {
        let agenda = agenda_with(&[("Q", AgendaKind::Investigation)]);
        let ledger = CuriosityLedger::open_in_memory().unwrap();
        let report = run_once(
            &agenda,
            &ledger,
            &Fake::answering(result),
            Autonomy::Proactive,
            &limits,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(report.receipt.outcome, expected);
        assert!(report.observation.is_none());
        assert_eq!(ledger.runs_today(Utc::now()).unwrap(), 1);
    }
}

#[tokio::test]
async fn a_run_past_its_time_limit_is_cancelled_and_recorded() {
    let agenda = agenda_with(&[("Q", AgendaKind::Investigation)]);
    let ledger = CuriosityLedger::open_in_memory().unwrap();
    let hanging = Fake {
        result: Mutex::new(None),
        seen: Mutex::new(Vec::new()),
        hang: true,
    };
    let limits = CuriosityLimits {
        max_run_secs: 1,
        ..CuriosityLimits::default()
    };
    let cancel = CancellationToken::new();
    let report = run_once(
        &agenda,
        &ledger,
        &hanging,
        Autonomy::Balanced,
        &limits,
        cancel.clone(),
    )
    .await
    .unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(report.receipt.outcome, RunOutcome::Cancelled);
}

#[tokio::test]
async fn findings_are_bounded() {
    let agenda = agenda_with(&[("Q", AgendaKind::Investigation)]);
    let ledger = CuriosityLedger::open_in_memory().unwrap();
    let huge = Finding {
        summary: "x".repeat(10_000),
        sources: (0..20).map(|i| format!("source {i}")).collect(),
        ..finding()
    };
    let report = run_once(
        &agenda,
        &ledger,
        &Fake::answering(Ok(huge)),
        Autonomy::Balanced,
        &CuriosityLimits::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let Some(sail_discovery::detect::Observation::CuriosityFinding {
        summary, sources, ..
    }) = report.observation
    else {
        panic!("expected a finding");
    };
    assert!(summary.chars().count() <= 1_200);
    assert_eq!(sources.len(), 6);
}
