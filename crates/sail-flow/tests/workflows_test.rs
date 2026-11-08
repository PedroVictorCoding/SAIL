use sail_flow::workflows::{Error, Store, validate};
use sail_protocol::flow::*;
use uuid::Uuid;
fn workflow() -> Workflow {
    serde_json::from_value(serde_json::json!({"version":1,"id":Uuid::new_v4(),"revision":1,"name":"Branching text","nodes":[
 {"id":"trigger","source":null,"operation":{"kind":"trigger"}},
 {"id":"trim","source":"trigger","operation":{"kind":"trim"}},
 {"id":"check","source":"trim","operation":{"kind":"contains","text":"hello"}},
 {"id":"upper","source":"check","branch":"true","operation":{"kind":"uppercase"}},
 {"id":"yes","source":"upper","operation":{"kind":"output"}},
 {"id":"no","source":"check","branch":"false","operation":{"kind":"output"}}
]})).unwrap()
}
fn store() -> (std::path::PathBuf, Store) {
    let p = std::env::temp_dir()
        .join(format!("flow-test-{}", Uuid::new_v4()))
        .join("state.json");
    let s = Store::open(p.clone()).unwrap();
    (p, s)
}
fn save(s: &Store, w: Workflow) {
    s.save(SaveWorkflow {
        expected_revision: 0,
        workflow: w,
    })
    .unwrap();
}
fn start(s: &Store, w: &Workflow, input: &str, id: Uuid) -> Run {
    s.start(StartRun {
        id,
        workflow_id: w.id,
        revision: w.revision,
        input: input.into(),
    })
    .unwrap()
}
#[test]
fn deterministic_branches_and_deduplication_survive_restart() {
    let (path, s) = store();
    let w = workflow();
    save(&s, w.clone());
    let id = Uuid::new_v4();
    start(&s, &w, "  hello  ", id);
    assert!(s.step(id).unwrap());
    assert!(s.step(id).unwrap());
    drop(s);
    let s = Store::open(path.clone()).unwrap();
    assert_eq!(s.list().unwrap().1[0].state, RunState::Paused);
    assert!(!s.step(id).unwrap());
    s.control(id, true).unwrap();
    while s.step(id).unwrap() {}
    let r = s.list().unwrap().1.remove(0);
    assert_eq!(r.state, RunState::Completed);
    assert_eq!(r.receipts.len(), 6);
    assert_eq!(r.receipts[4].output, "HELLO");
    assert!(r.receipts[5].skipped);
    assert_eq!(start(&s, &w, "  hello  ", id).receipts.len(), 6);
    assert!(matches!(
        s.start(StartRun {
            id,
            workflow_id: w.id,
            revision: 1,
            input: "other".into()
        }),
        Err(Error::Conflict(_))
    ));
    let second = Uuid::new_v4();
    start(&s, &w, "  hello  ", second);
    while s.step(second).unwrap() {}
    assert_eq!(
        s.list().unwrap().1[1]
            .receipts
            .iter()
            .map(|p| (&p.output, p.skipped))
            .collect::<Vec<_>>(),
        r.receipts
            .iter()
            .map(|p| (&p.output, p.skipped))
            .collect::<Vec<_>>()
    );
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
#[test]
fn cancellation_is_terminal_and_versions_are_immutable() {
    let (path, s) = store();
    let mut w = workflow();
    save(&s, w.clone());
    let id = Uuid::new_v4();
    start(&s, &w, "bye", id);
    s.step(id).unwrap();
    s.control(id, false).unwrap();
    assert!(!s.step(id).unwrap());
    assert!(s.control(id, true).is_err());
    assert!(
        s.save(SaveWorkflow {
            expected_revision: 0,
            workflow: w.clone()
        })
        .is_err()
    );
    w.revision = 2;
    w.name = "Revision two".into();
    s.save(SaveWorkflow {
        expected_revision: 1,
        workflow: w.clone(),
    })
    .unwrap();
    assert_eq!(s.list().unwrap().0[0].revision, 2);
    assert_eq!(s.list().unwrap().1[0].workflow.revision, 1);
    let id = Uuid::new_v4();
    start(&s, &w, "bye", id);
    while s.step(id).unwrap() {}
    let r = s.list().unwrap().1.remove(1);
    assert!(r.receipts[4].skipped);
    assert_eq!(r.receipts[5].output, "bye");
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
#[test]
fn invalid_graphs_and_unknown_effectful_nodes_are_rejected() {
    let mut w = workflow();
    w.nodes[1].source = Some("check".into());
    assert!(validate(&w).is_err());
    let mut w = workflow();
    w.nodes[1].source = Some("missing".into());
    assert!(validate(&w).is_err());
    let mut w = workflow();
    w.nodes[1].branch = Branch::True;
    assert!(validate(&w).is_err());
    let mut w = workflow();
    w.version = 99;
    assert!(validate(&w).is_err());
    assert!(
        serde_json::from_value::<Operation>(serde_json::json!({"kind":"shell","command":"id"}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<Operation>(serde_json::json!({"kind":"trim","command":"id"}))
            .is_err()
    );
}
#[test]
fn resource_limits_fail_honestly_and_disk_errors_do_not_commit() {
    let (path, s) = store();
    let mut w = workflow();
    w.nodes[1].operation = Operation::Uppercase {};
    save(&s, w.clone());
    // Unicode case expansion can exceed the per-node bound even with a valid input.
    let id = Uuid::new_v4();
    start(&s, &w, &"ΐ".repeat(2048), id);
    while s.step(id).unwrap() {}
    let r = s.list().unwrap().1.remove(0);
    assert_eq!(r.state, RunState::Failed);
    assert!(r.error.unwrap().contains("8192"));
    assert!(
        s.start(StartRun {
            id: Uuid::new_v4(),
            workflow_id: w.id,
            revision: 1,
            input: "x".repeat(4097)
        })
        .is_err()
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        s.start(StartRun {
            id: Uuid::new_v4(),
            workflow_id: w.id,
            revision: 1,
            input: "x".into()
        })
        .is_err()
    );
    assert_eq!(s.list().unwrap().1.len(), 1);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn retention_controls_require_terminal_runs() {
    let (path, s) = store();
    let w = workflow();
    save(&s, w.clone());
    let id = Uuid::new_v4();
    start(&s, &w, "text", id);
    assert!(s.delete_run(id).is_err());
    assert!(s.delete_workflow(w.id).is_err());
    s.control(id, false).unwrap();
    s.delete_workflow(w.id).unwrap();
    assert!(s.list().unwrap().0.is_empty());
    assert_eq!(s.list().unwrap().1.len(), 1);
    s.delete_run(id).unwrap();
    assert!(s.list().unwrap().1.is_empty());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
