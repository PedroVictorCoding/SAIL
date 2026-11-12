//! Ledger: usage by day/month, cache TTL, recovery, outcomes, pruning, persistence.

mod escalation_fixtures;

use chrono::{DateTime, TimeZone, Utc};
use escalation_fixtures::open_policy;
use sail_mind::escalation::store::EscalationStore;
use sail_mind::escalation::{
    Decision, DecisionRecord, EscalationJob, JobState, MAX_ATTEMPTS, MAX_STORED_BYTES, Origin,
    Outcome, TaskKind,
};

fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0)
        .single()
        .expect("valid test time")
}

fn job(id: &str, fingerprint: &str, kind: TaskKind, created_at: DateTime<Utc>) -> EscalationJob {
    EscalationJob {
        id: id.to_string(),
        fingerprint: fingerprint.to_string(),
        kind,
        origin: Origin::ModelTool,
        provider: "primary".to_string(),
        model: "primary-model".to_string(),
        budget: 1_000,
        state: JobState::Pending,
        attempts: 0,
        question: "the redacted question".to_string(),
        justification: "why".to_string(),
        expected_benefit: "benefit".to_string(),
        answer: None,
        error: None,
        input_tokens: 0,
        output_tokens: 0,
        cost_micros: 0,
        correlation_id: None,
        created_at,
        finished_at: None,
        outcome: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn finish(
    store: &EscalationStore,
    id: &str,
    fingerprint: &str,
    kind: TaskKind,
    finished_at: DateTime<Utc>,
    input_tokens: u64,
    output_tokens: u64,
    cost_micros: u64,
) {
    store
        .insert_job(&job(id, fingerprint, kind, finished_at))
        .expect("insert");
    store.mark_running(id).expect("running");
    store
        .finish_job(
            id,
            "the answer",
            "primary-model",
            input_tokens,
            output_tokens,
            cost_micros,
            finished_at,
        )
        .expect("finish");
}

#[test]
fn usage_totals_split_by_utc_day_and_month() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    finish(
        &store,
        "today",
        "fp-today",
        TaskKind::Coding,
        at(2026, 11, 12, 9),
        100,
        50,
        500,
    );
    finish(
        &store,
        "this-month",
        "fp-month",
        TaskKind::Research,
        at(2026, 11, 2, 9),
        200,
        100,
        700,
    );
    finish(
        &store,
        "last-month",
        "fp-last",
        TaskKind::Writing,
        at(2026, 10, 3, 9),
        400,
        200,
        900,
    );

    let usage = store.usage(&open_policy(), now).expect("usage");
    assert_eq!(usage.tokens_today, 150);
    assert_eq!(usage.calls_today, 1);
    assert_eq!(usage.cost_today_micros, 500);
    assert_eq!(usage.tokens_month, 450);
    assert_eq!(usage.cost_month_micros, 1_200);

    let later = at(2026, 12, 1, 0);
    let next_month = store.usage(&open_policy(), later).expect("usage");
    assert_eq!(next_month.tokens_today, 0);
    assert_eq!(next_month.tokens_month, 0);
}

#[test]
fn usage_reports_rate_limits_and_in_flight_fingerprints() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    store
        .insert_job(&job("pending", "fp-pending", TaskKind::Planning, now))
        .expect("insert");
    store.mark_running("pending").expect("running");
    let until = now + chrono::Duration::minutes(10);
    store.block_provider("primary", until).expect("block");

    let usage = store.usage(&open_policy(), now).expect("usage");
    assert_eq!(usage.in_flight, vec!["fp-pending".to_string()]);
    assert_eq!(usage.blocked_until, vec![("primary".to_string(), until)]);

    let later = now + chrono::Duration::hours(1);
    assert!(
        store
            .usage(&open_policy(), later)
            .expect("usage")
            .blocked_until
            .is_empty()
    );
}

#[test]
fn recent_fingerprints_follow_the_cooldown_window() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    finish(
        &store,
        "recent",
        "fp-recent",
        TaskKind::Coding,
        now - chrono::Duration::minutes(30),
        10,
        10,
        1,
    );
    finish(
        &store,
        "old",
        "fp-old",
        TaskKind::Coding,
        now - chrono::Duration::hours(3),
        10,
        10,
        1,
    );

    let mut policy = open_policy();
    policy.cooldown_secs = 3_600;
    let usage = store.usage(&policy, now).expect("usage");
    assert_eq!(usage.recent.len(), 1);
    assert_eq!(usage.recent[0].0, "fp-recent");

    policy.cooldown_secs = 0;
    assert!(store.usage(&policy, now).expect("usage").recent.is_empty());
}

#[test]
fn cache_ttl_is_measured_from_the_finish_time() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    finish(
        &store,
        "cached",
        "fp-cached",
        TaskKind::Research,
        now,
        10,
        10,
        1,
    );

    let hit = store
        .cached("fp-cached", 3_600, now + chrono::Duration::seconds(30))
        .expect("cached");
    let hit = hit.expect("within the TTL");
    assert_eq!(hit.id, "cached");
    assert_eq!(hit.answer.as_deref(), Some("the answer"));
    assert_eq!(hit.state, JobState::Done);

    assert!(
        store
            .cached("fp-cached", 60, now + chrono::Duration::seconds(120))
            .expect("cached")
            .is_none()
    );
    assert!(store.cached("fp-cached", 0, now).expect("cached").is_none());
    assert!(
        store
            .cached("fp-other", 3_600, now)
            .expect("cached")
            .is_none()
    );
}

#[test]
fn recovery_requeues_running_jobs_and_gives_up_after_max_attempts() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    store
        .insert_job(&job("pending", "fp-pending", TaskKind::Coding, now))
        .expect("insert");
    store
        .insert_job(&job("running", "fp-running", TaskKind::Coding, now))
        .expect("insert");
    store.mark_running("running").expect("running");
    store
        .insert_job(&job("exhausted", "fp-exhausted", TaskKind::Coding, now))
        .expect("insert");
    for _ in 0..MAX_ATTEMPTS {
        store.mark_running("exhausted").expect("running");
    }
    finish(&store, "done", "fp-done", TaskKind::Coding, now, 1, 1, 1);

    let pending = store.recover().expect("recover");
    let ids: Vec<&str> = pending.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids, vec!["pending", "running"]);
    assert_eq!(
        store.job("running").expect("job").expect("exists").state,
        JobState::Pending
    );
    assert_eq!(
        store.job("exhausted").expect("job").expect("exists").state,
        JobState::Failed
    );
    assert_eq!(
        store.job("done").expect("job").expect("exists").state,
        JobState::Done
    );
}

#[test]
fn fail_job_retries_until_the_attempt_limit() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    store
        .insert_job(&job("retry", "fp-retry", TaskKind::Coding, now))
        .expect("insert");
    assert_eq!(
        store.fail_job("retry", "network", true, now).expect("fail"),
        JobState::Pending
    );
    store.mark_running("retry").expect("running");
    assert_eq!(
        store
            .fail_job("retry", "network", false, now)
            .expect("fail"),
        JobState::Failed
    );
    let job = store.job("retry").expect("job").expect("exists");
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error.as_deref(), Some("network"));
    assert!(job.finished_at.is_some());
    assert!(store.fail_job("missing", "x", false, now).is_err());
}

#[test]
fn outcomes_tally_improved_and_not_improved_per_kind() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    finish(&store, "a", "fp-a", TaskKind::Coding, now, 1, 1, 1);
    finish(&store, "b", "fp-b", TaskKind::Coding, now, 1, 1, 1);
    finish(&store, "c", "fp-c", TaskKind::Research, now, 1, 1, 1);
    store
        .record_outcome("a", Outcome::Improved)
        .expect("outcome");
    store
        .record_outcome("b", Outcome::NoChange)
        .expect("outcome");
    store.record_outcome("c", Outcome::Worse).expect("outcome");
    store
        .record_outcome("c", Outcome::Improved)
        .expect("replace");

    let stats = store.outcomes().expect("outcomes");
    assert_eq!(stats.tally(TaskKind::Coding), (1, 1));
    assert_eq!(stats.tally(TaskKind::Research), (1, 0));
    assert_eq!(stats.tally(TaskKind::Writing), (0, 0));

    let job = store.job("c").expect("job").expect("exists");
    assert_eq!(job.outcome, Some(Outcome::Improved));
    assert!(store.record_outcome("missing", Outcome::Improved).is_err());
}

#[test]
fn decisions_round_trip_newest_first() {
    let store = EscalationStore::open_in_memory().expect("store");
    let start = at(2026, 11, 12, 8);
    for (index, decision) in [
        Decision::Escalate {
            provider: "primary".into(),
            budget: 100,
        },
        Decision::Deny {
            reason: sail_mind::escalation::DenyReason::BelowThreshold,
        },
        Decision::Cached {
            job_id: "job-1".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .log_decision(&DecisionRecord {
                at: start + chrono::Duration::minutes(index as i64),
                kind: TaskKind::Reasoning,
                origin: Origin::ModelTool,
                fingerprint: format!("fp-{index}"),
                decision,
                route_score: 0.25 * index as f32,
                job_id: None,
            })
            .expect("log");
    }
    let records = store.decisions(2).expect("decisions");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].fingerprint, "fp-2");
    assert_eq!(records[1].fingerprint, "fp-1");
    assert_eq!(store.decisions(10).expect("decisions").len(), 3);
}

#[test]
fn prune_drops_old_rows_and_expired_cache_only() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    finish(
        &store,
        "old",
        "fp-old",
        TaskKind::Coding,
        now - chrono::Duration::days(40),
        10,
        10,
        5,
    );
    finish(
        &store,
        "new",
        "fp-new",
        TaskKind::Coding,
        now - chrono::Duration::hours(1),
        10,
        10,
        5,
    );
    for days_ago in [40, 0] {
        store
            .log_decision(&DecisionRecord {
                at: now - chrono::Duration::days(days_ago),
                kind: TaskKind::Coding,
                origin: Origin::ModelTool,
                fingerprint: "fp".into(),
                decision: Decision::Deny {
                    reason: sail_mind::escalation::DenyReason::Disabled,
                },
                route_score: 0.0,
                job_id: None,
            })
            .expect("log");
    }

    store.prune(30, now).expect("prune");
    let usage = store.usage(&open_policy(), now).expect("usage");
    assert_eq!(usage.tokens_today, 20);
    assert_eq!(usage.tokens_month, 20);
    assert_eq!(store.decisions(10).expect("decisions").len(), 1);
    assert!(
        store
            .cached("fp-old", u64::MAX / 2, now)
            .expect("cached")
            .is_none(),
        "expired cache row must not be served"
    );
    assert!(
        store
            .cached("fp-new", 3_600, now)
            .expect("cached")
            .is_some()
    );
}

#[test]
fn text_fields_are_bounded_and_jobs_list_newest_first() {
    let store = EscalationStore::open_in_memory().expect("store");
    let now = at(2026, 11, 12, 12);
    let mut huge = job("huge", "fp-huge", TaskKind::Coding, now);
    huge.question = "a".repeat(MAX_STORED_BYTES * 2);
    huge.justification = "b".repeat(MAX_STORED_BYTES * 2);
    huge.expected_benefit = "c".repeat(MAX_STORED_BYTES * 2);
    store.insert_job(&huge).expect("insert");
    let stored = store.job("huge").expect("job").expect("exists");
    assert_eq!(stored.question.len(), MAX_STORED_BYTES);
    assert_eq!(stored.justification.len(), MAX_STORED_BYTES);
    assert_eq!(stored.expected_benefit.len(), MAX_STORED_BYTES);

    store
        .insert_job(&job(
            "older",
            "fp-older",
            TaskKind::Coding,
            now - chrono::Duration::hours(1),
        ))
        .expect("insert");
    let listed = store.jobs(10).expect("jobs");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, "huge");
    assert_eq!(listed[1].id, "older");
}

#[test]
fn store_survives_reopen() {
    let path =
        std::env::temp_dir().join(format!("sail-escalation-test-{}.db", uuid::Uuid::new_v4()));
    let now = at(2026, 11, 12, 12);
    {
        let store = EscalationStore::open(&path).expect("open");
        finish(
            &store,
            "kept",
            "fp-kept",
            TaskKind::Planning,
            now,
            12,
            34,
            56,
        );
        store
            .record_outcome("kept", Outcome::Improved)
            .expect("outcome");
    }
    {
        let store = EscalationStore::open(&path).expect("reopen");
        let job = store.job("kept").expect("job").expect("exists");
        assert_eq!(job.state, JobState::Done);
        assert_eq!(job.answer.as_deref(), Some("the answer"));
        assert_eq!(job.input_tokens, 12);
        assert_eq!(job.outcome, Some(Outcome::Improved));
        assert!(
            store
                .cached("fp-kept", 3_600, now)
                .expect("cached")
                .is_some()
        );
        assert_eq!(
            store
                .usage(&open_policy(), now)
                .expect("usage")
                .tokens_today,
            46
        );
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[test]
fn the_schema_version_is_one() {
    let path = std::env::temp_dir().join(format!(
        "sail-escalation-schema-{}.db",
        uuid::Uuid::new_v4()
    ));
    drop(EscalationStore::open(&path).expect("open"));
    let conn = rusqlite::Connection::open(&path).expect("reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(version, 1);
    drop(conn);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}
