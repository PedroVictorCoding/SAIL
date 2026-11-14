use sail_flow::schedule::{
    Action, Creator, Deliver, Due, Job, JobState, MAX_JOBS, MAX_MODEL_JOBS, Outcome, Store, finish,
    plan,
};
use std::fs;
use tempfile::TempDir;

const A: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

fn id(value: u64) -> String {
    format!("{value:032x}")
}

fn new_store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store {
        dir: dir.path().join("schedule"),
    };
    (dir, store)
}

fn job(value: u64, when: &str) -> Job {
    Job {
        id: id(value),
        name: format!("job {value}"),
        when_text: when.into(),
        tz: "Europe/Lisbon".into(),
        anchor: A,
        action: Action::Prompt {
            text: "do the thing".into(),
        },
        deliver: Deliver::Web,
        state: JobState::Active,
        created_by: Creator::Owner,
        created_at: A,
        last_slot: None,
        failure_streak: 0,
        reason: None,
    }
}

#[test]
fn create_validates_every_limit() {
    let (_dir, store) = new_store();
    let created = store.create(job(1, "every day")).unwrap();
    assert_eq!(created.id, id(1));
    assert_eq!(created.name, "job 1");

    let mut padded = job(2, "every day");
    padded.name = "  padded  ".into();
    assert_eq!(store.create(padded).unwrap().name, "padded");

    let mut bad = job(3, "every day");
    bad.name = String::new();
    assert!(store.create(bad).is_err());
    let mut bad = job(4, "every day");
    bad.name = "x".repeat(81);
    assert!(store.create(bad).is_err());
    let mut bad = job(5, "every day");
    bad.name = "bad\nname".into();
    assert!(store.create(bad).is_err());
    let mut bad = job(6, "every day");
    bad.id = format!("{}A", "0".repeat(31));
    assert!(store.create(bad).is_err());
    let mut bad = job(7, "every day");
    bad.id = "abc".into();
    assert!(store.create(bad).is_err());
    let mut bad = job(8, "evry day");
    assert!(store.create(bad.clone()).is_err());
    bad.when_text = "every day".into();
    bad.action = Action::Prompt {
        text: String::new(),
    };
    assert!(store.create(bad).is_err());
    let mut bad = job(9, "every day");
    bad.action = Action::Prompt {
        text: "x".repeat(4001),
    };
    assert!(store.create(bad).is_err());
    let mut bad = job(10, "every day");
    bad.action = Action::Skill {
        name: "Not A Name".into(),
        input: String::new(),
    };
    assert!(store.create(bad).is_err());
    let mut bad = job(11, "every day");
    bad.action = Action::Skill {
        name: "good-name".into(),
        input: "x".repeat(4001),
    };
    assert!(store.create(bad).is_err());
    let mut bad = job(12, "every day");
    bad.action = Action::Workflow { id: String::new() };
    assert!(store.create(bad).is_err());
    let mut bad = job(13, "every day");
    bad.action = Action::Workflow {
        id: "x".repeat(129),
    };
    assert!(store.create(bad).is_err());
    let mut bad = job(14, "every day");
    bad.action = Action::Workflow {
        id: "bad/id".into(),
    };
    assert!(store.create(bad).is_err());
    assert!(store.create(job(1, "every day")).is_err());
    let mut same_name = job(15, "every day");
    same_name.name = "JOB 1".into();
    assert!(store.create(same_name).is_err());

    let mut model = job(16, "every day");
    model.created_by = Creator::Model;
    model.state = JobState::Proposed;
    assert!(store.create(model.clone()).is_err());
    model.reason = Some("x".repeat(501));
    assert!(store.create(model).is_err());
    model = job(17, "every day");
    model.created_by = Creator::Model;
    model.state = JobState::Proposed;
    model.reason = Some("the owner asked for a summary".into());
    assert!(store.create(model).is_ok());

    let mut missing = job(999, "every day");
    missing.id = id(999);
    assert!(store.update(&missing).is_err());
    let mut change = store.get(&id(1)).unwrap();
    change.name = "renamed".into();
    store.update(&change).unwrap();
    assert_eq!(store.get(&id(1)).unwrap().name, "renamed");
}

#[test]
fn max_jobs_and_max_model_jobs_are_enforced() {
    let (_dir, store) = new_store();
    for value in 0..(MAX_JOBS as u64) {
        let mut value_job = job(1000 + value, "every day");
        value_job.name = format!("owner {value}");
        store.create(value_job).unwrap();
    }
    let mut extra = job(9000, "every day");
    extra.name = "one too many".into();
    assert!(store.create(extra).is_err());

    let (_dir, store) = new_store();
    for value in 0..(MAX_MODEL_JOBS as u64) {
        let mut model = job(2000 + value, "every day");
        model.name = format!("model {value}");
        model.created_by = Creator::Model;
        model.state = JobState::Proposed;
        model.reason = Some("why".into());
        store.create(model).unwrap();
    }
    let mut extra = job(3000, "every day");
    extra.created_by = Creator::Model;
    extra.state = JobState::Proposed;
    extra.reason = Some("why".into());
    assert!(store.create(extra).is_err());
}

#[test]
fn plan_collapses_a_downtime_into_one_missed_run_and_one_due() {
    let mut value = job(1, "every 1h");
    value.last_slot = Some(A);
    let now = A + 3 * 86_400 + 30;
    let first = plan(&[value.clone()], now, 600);
    let second = plan(&[value], now, 600);
    assert_eq!(first, second);
    assert_eq!(first.due.len(), 1);
    assert_eq!(first.missed.len(), 1);
    assert_eq!(first.due[0].slot, A + 3 * 86_400);
    assert!(!first.due[0].catch_up);
    assert_eq!(first.missed[0].slot, A + 3 * 86_400 - 3600);
    assert!(
        first.missed[0]
            .summary
            .starts_with("missed 71 runs between ")
    );
    assert!(first.missed[0].summary.ends_with("Z"));
    assert_eq!(first.missed[0].outcome, Outcome::Missed);

    let late = plan(&[job(2, "every 1h")], A + 3600 + 120, 600);
    assert!(late.due[0].catch_up);
}

#[test]
fn grace_exactly_at_the_edge_is_due() {
    let mut value = job(1, "every 1h");
    value.last_slot = Some(A);
    let edge = plan(&[value.clone()], A + 3600 + 600, 600);
    assert_eq!(edge.due.len(), 1);
    assert!(edge.missed.is_empty());
    let past = plan(&[value], A + 3600 + 601, 600);
    assert!(past.due.is_empty());
    assert_eq!(past.missed.len(), 1);
    assert_eq!(past.missed[0].slot, A + 3600);
}

#[test]
fn starting_a_slot_twice_is_refused_and_two_ticks_do_not_repeat() {
    let (_dir, store) = new_store();
    let value = store.create(job(1, "every 5 minutes")).unwrap();
    let due = Due {
        job_id: value.id.clone(),
        slot: A + 300,
        catch_up: false,
    };
    let run = store.start(&due, A + 301).unwrap();
    assert_eq!(run.outcome, Outcome::Running);
    assert!(store.start(&due, A + 302).is_err());
    let again = plan(&store.jobs().unwrap(), A + 302, 600);
    assert!(again.due.is_empty());
    let later = plan(&store.jobs().unwrap(), A + 601, 600);
    assert_eq!(later.due.len(), 1);
    assert_eq!(later.due[0].slot, A + 600);
}

#[test]
fn a_once_job_becomes_done() {
    let (_dir, store) = new_store();
    let value = store.create(job(1, "in 30m")).unwrap();
    let due = Due {
        job_id: value.id.clone(),
        slot: A + 1800,
        catch_up: false,
    };
    store.start(&due, A + 1801).unwrap();
    let run_id = format!("{}:{}", value.id, A + 1800);
    let (run, notify) = store
        .finish_run(&run_id, Outcome::Ok, "done", A + 1802)
        .unwrap();
    assert_eq!(run.outcome, Outcome::Ok);
    assert_eq!(run.delivered_at, None);
    assert!(!notify);
    assert_eq!(store.get(&value.id).unwrap().state, JobState::Done);
    assert!(plan(&store.jobs().unwrap(), A + 9999, 600).due.is_empty());
    store.mark_delivered(&run_id, A + 1803).unwrap();
    assert_eq!(
        store.runs(&value.id).unwrap()[0].delivered_at,
        Some(A + 1803)
    );
}

#[test]
fn a_failure_streak_pauses_the_job_and_asks_to_tell_the_owner() {
    let (_dir, store) = new_store();
    let value = store.create(job(1, "every 1h")).unwrap();
    for (index, slot) in [A + 3600, A + 7200, A + 10800].into_iter().enumerate() {
        let due = Due {
            job_id: value.id.clone(),
            slot,
            catch_up: false,
        };
        store.start(&due, slot + 1).unwrap();
        let (_, notify) = store
            .finish_run(
                &format!("{}:{slot}", value.id),
                Outcome::Failed,
                "boom",
                slot + 2,
            )
            .unwrap();
        assert_eq!(notify, index == 2);
    }
    let paused = store.get(&value.id).unwrap();
    assert_eq!(paused.state, JobState::Paused);
    assert_eq!(paused.failure_streak, 3);
    assert!(plan(&[paused], A + 99_999, 600).due.is_empty());
}

#[test]
fn finish_is_pure_and_deterministic() {
    let mut first = job(1, "every day");
    let mut second = first.clone();
    let one = finish(&mut first, &Outcome::Failed);
    let two = finish(&mut second, &Outcome::Failed);
    assert_eq!(one, two);
    assert_eq!(first, second);
}

#[test]
fn recover_fails_crashed_runs_and_is_idempotent() {
    let (_dir, store) = new_store();
    let value = store.create(job(1, "every 1h")).unwrap();
    for slot in [A + 3600, A + 7200, A + 10800] {
        store
            .start(
                &Due {
                    job_id: value.id.clone(),
                    slot,
                    catch_up: false,
                },
                slot,
            )
            .unwrap();
    }
    assert_eq!(store.recover(A + 20_000).unwrap(), 3);
    assert_eq!(store.recover(A + 20_001).unwrap(), 0);
    let runs = store.runs(&value.id).unwrap();
    assert_eq!(runs.len(), 3);
    assert!(runs.iter().all(|run| run.outcome == Outcome::Failed));
    assert!(runs.iter().all(|run| run.finished_at == Some(A + 20_000)));
    assert!(
        runs.iter()
            .all(|run| run.summary == "SAIL stopped during this run")
    );
    assert_eq!(store.get(&value.id).unwrap().state, JobState::Paused);
}

#[test]
fn only_the_newest_fifty_runs_are_kept() {
    let (_dir, store) = new_store();
    let value = store.create(job(1, "every 5 minutes")).unwrap();
    for slot in 0..60 {
        store
            .start(
                &Due {
                    job_id: value.id.clone(),
                    slot: A + 300 * (slot + 1),
                    catch_up: false,
                },
                A + 300 * (slot + 1),
            )
            .unwrap();
    }
    let runs = store.runs(&value.id).unwrap();
    assert_eq!(runs.len(), 50);
    assert_eq!(runs[0].slot, A + 300 * 60);
    assert_eq!(runs[49].slot, A + 300 * 11);
}

#[test]
fn corrupt_jobs_are_an_error_and_are_not_replaced() {
    let (_dir, store) = new_store();
    fs::create_dir_all(&store.dir).unwrap();
    let path = store.dir.join("jobs.json");
    fs::write(&path, b"not json").unwrap();
    let before = fs::read(&path).unwrap();
    assert!(store.jobs().is_err());
    assert!(store.create(job(1, "every day")).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn a_proposed_job_is_never_planned() {
    let mut value = job(1, "in 30m");
    value.state = JobState::Proposed;
    value.created_by = Creator::Model;
    value.reason = Some("try this".into());
    let plan = plan(&[value], A + 1801, 600);
    assert!(plan.due.is_empty());
    assert!(plan.missed.is_empty());
}

#[test]
fn summaries_are_cut_on_a_char_boundary() {
    let (_dir, store) = new_store();
    let value = store.create(job(1, "in 30m")).unwrap();
    let slot = A + 1800;
    store
        .start(
            &Due {
                job_id: value.id.clone(),
                slot,
                catch_up: false,
            },
            slot,
        )
        .unwrap();
    let (run, _) = store
        .finish_run(
            &format!("{}:{slot}", value.id),
            Outcome::Ok,
            &"é".repeat(3000),
            slot + 1,
        )
        .unwrap();
    assert_eq!(run.summary.chars().count(), 2000);
    assert!(run.summary.ends_with('…'));
}

#[test]
fn delete_drops_the_job_and_its_runs() {
    let (_dir, store) = new_store();
    let value = store.create(job(1, "in 30m")).unwrap();
    let slot = A + 1800;
    store
        .start(
            &Due {
                job_id: value.id.clone(),
                slot,
                catch_up: false,
            },
            slot,
        )
        .unwrap();
    store.delete(&value.id).unwrap();
    assert!(store.get(&value.id).is_err());
    assert!(store.runs(&value.id).unwrap().is_empty());
    assert!(store.delete(&value.id).is_err());
}
