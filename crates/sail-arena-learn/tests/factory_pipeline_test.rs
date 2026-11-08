//! Course Factory acceptance: a Markdown book becomes a published, served lesson through the
//! real queue, pipeline, validators, registry and course engine. Only the model is scripted;
//! it answers by reply schema, the way the local model would be asked.
use sail_arena_learn::courses::content::Domain;
use sail_arena_learn::courses::{CourseEngine, CourseStore, NoSandbox};
use sail_arena_learn::factory::contracts::SourceRights;
use sail_arena_learn::factory::contracts::{
    AuthorModel, AuthorModelError, AuthorPrompt, LessonStage, ProjectStatus,
};
use sail_arena_learn::factory::jobs::{JobQueue, ModelGate, Tick, Worker};
use sail_arena_learn::factory::pipeline::{Pipeline, PipelineDeps, published_lesson_id};
use sail_arena_learn::factory::registry::PackageRegistry;
use sail_arena_learn::factory::service::{FactoryService, ImportRequest, NewCourse};
use sail_arena_learn::factory::settings::SettingsStore;
use sail_arena_learn::factory::sources::SourceRegistry;
use sail_arena_learn::factory::store::FactoryStore;
use sail_arena_learn::factory::validate::ValidationSuite;
use sail_continuity::Continuity;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const BOOK: &[u8] = b"# Ownership\n\n\
Every value in a Rust program has a single owner, the variable that holds it.\n\n\
When the owner goes out of scope the value is dropped and its memory is freed.\n\n\
## Moves\n\n\
Assigning a heap value to another variable moves ownership; the first variable \
can no longer be used.\n";

const CONCEPT: &str = "rust.ownership";
const OBJECTIVE: &str = "rust.ownership.objective.1";

/// Replies by schema. `fragment` is filled in once the book is imported, because the
/// model may only cite fragments that were in its prompt.
struct ScriptedModel {
    fragment: Mutex<String>,
    calls: AtomicUsize,
    /// Fail the first N lesson calls with an unparseable reply.
    bad_lessons: AtomicUsize,
}

impl ScriptedModel {
    fn lesson(&self) -> String {
        let f = self.fragment.lock().unwrap().clone();
        let step = |id: &str, family: &str, title: &str| {
            serde_json::json!({
                "id": id, "kind": "choice", "family": family, "title": title,
                "objective_ids": [OBJECTIVE], "confidence": false,
                "body": [{"kind": "p", "text": "A String is assigned from s1 to s2. Which variable can still be used?"}],
                "options": [{"id": "a", "text": "Only s2"}, {"id": "b", "text": "Both of them"}],
                "multi": false, "key": {"choice": ["a"]},
                "hints": ["Think about who frees the memory.", "Only one variable can own it."],
                "correct": [{"kind": "p", "text": "The value moved to s2."}],
                "solution": [{"kind": "p", "text": "After the move only s2 owns the String."}],
                "misconceptions": {}
            })
        };
        serde_json::json!({
            "lesson": {
                "id": "model-chose-this", "title": "Who owns it", "minutes": 5,
                "objective_ids": [OBJECTIVE], "prerequisites": [],
                "steps": [
                    step("s1", "own.basics", "Who owns the String"),
                    step("s2", "own.transfer", "Ownership in a new setting"),
                ]
            },
            "claims": [{"text": "A value has one owner at a time, and assignment moves it.", "fragment_ids": [f]}],
            "rust": [], "numeric": [], "rubric": [],
            "review_variants": [step("r1", "own.review", "Moved values")],
            "follow_up": null, "limitations": []
        })
        .to_string()
    }
}

#[async_trait::async_trait]
impl AuthorModel for ScriptedModel {
    async fn complete(
        &self,
        prompt: &AuthorPrompt,
        _cancel: CancellationToken,
    ) -> Result<String, AuthorModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let f = self.fragment.lock().unwrap().clone();
        Ok(match prompt.schema {
            "concepts" => serde_json::json!({"concepts": [{
                "id": CONCEPT, "name": "Ownership",
                "description": "Each value has one owner that frees it.",
                "objectives": ["The learner can tell which variable owns a value after a move."],
                "prerequisites": {}, "fragment_ids": [f],
                "difficulty": 2, "validator": "exact_key"
            }]})
            .to_string(),
            "curriculum" => serde_json::json!({
                "title": "Ownership basics", "level": "Beginner",
                "scope": "Owners, scope and moves.", "outcomes": ["Explain moves."],
                "units": [{"id": "unit.1", "title": "Owners", "objective_ids": [OBJECTIVE],
                    "lessons": [{"id": "lesson.1", "title": "Who owns it",
                        "objective_ids": [OBJECTIVE], "minutes": 5, "kind": "lesson"}],
                    "review_checkpoint": true}],
                "conflicts": []
            })
            .to_string(),
            "lesson" => {
                let garble = {
                    let left = self.bad_lessons.load(Ordering::SeqCst);
                    if left > 0 {
                        self.bad_lessons.store(left - 1, Ordering::SeqCst);
                    }
                    left > 0
                };
                if garble {
                    "Sure! Here is a lesson about ownership.".to_string()
                } else {
                    self.lesson()
                }
            }
            other => {
                return Err(AuthorModelError::Failed(format!(
                    "unexpected schema {other}"
                )));
            }
        })
    }
}

struct Open;
impl ModelGate for Open {
    fn admit_background(&self) -> Result<(), String> {
        Ok(())
    }
}

struct Rig {
    service: FactoryService,
    worker: Worker,
    engine: Arc<CourseEngine>,
    model: Arc<ScriptedModel>,
    store: Arc<FactoryStore>,
    clock: Arc<AtomicI64>,
    _dir: tempdir::Dir,
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn rig() -> Rig {
    let dir = std::env::temp_dir().join(format!("sail-cf-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let time = Arc::new(AtomicI64::new(1_700_000_000_000));
    let t = time.clone();
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> =
        Arc::new(move || t.fetch_add(1, Ordering::SeqCst));
    let sources = Arc::new(SourceRegistry::open_in_memory(dir.join("blobs")).unwrap());
    let store = Arc::new(FactoryStore::open_in_memory().unwrap());
    let settings = Arc::new(SettingsStore::open_in_memory().unwrap());
    let jobs = Arc::new(JobQueue::new(Arc::new(
        Continuity::open_in_memory().unwrap(),
    )));
    let engine = Arc::new(CourseEngine::new(
        CourseStore::open_in_memory().unwrap(),
        Arc::new(NoSandbox),
    ));
    let model = Arc::new(ScriptedModel {
        fragment: Mutex::new(String::new()),
        calls: AtomicUsize::new(0),
        bad_lessons: AtomicUsize::new(0),
    });
    let pipeline = Arc::new(
        Pipeline::new(
            rusqlite::Connection::open_in_memory().unwrap(),
            PipelineDeps {
                sources: sources.clone(),
                store: store.clone(),
                packages: Arc::new(PackageRegistry::open_in_memory().unwrap()),
                engine: engine.clone(),
                settings: settings.clone(),
                model: model.clone(),
                pdf: None,
                validators: Arc::new(ValidationSuite::new(Arc::new(NoSandbox))),
                clock: clock.clone(),
            },
        )
        .unwrap(),
    );
    let service = FactoryService::new(sources, jobs.clone(), settings.clone(), clock.clone())
        .with_pipeline(pipeline.clone());
    let worker = Worker::new(jobs, settings, pipeline, Arc::new(Open), clock);
    Rig {
        service,
        worker,
        engine,
        model,
        store,
        clock: time,
        _dir: tempdir::Dir(dir),
    }
}

/// Run jobs until the queue is idle, skipping past any backoff by advancing the clock.
async fn drain(rig: &Rig) -> Vec<Tick> {
    let cancel = CancellationToken::new();
    let mut ticks = Vec::new();
    for _ in 0..50 {
        let tick = rig.worker.tick(&cancel).await;
        if matches!(tick, Tick::Idle(_)) {
            if rig.service.jobs().active().unwrap().is_empty() {
                break;
            }
            rig.clock.fetch_add(7 * 60 * 60_000, Ordering::SeqCst);
            continue;
        }
        ticks.push(tick);
    }
    ticks
}

fn import_book(rig: &Rig) -> String {
    let (source, _) = rig
        .service
        .import(
            &ImportRequest {
                file_name: "ownership.md".into(),
                title: Some("Ownership notes".into()),
                rights: SourceRights::Restricted,
                scope: "learning:rust".into(),
            },
            BOOK,
        )
        .unwrap();
    let fragments = rig.service.sources().fragments(&source.source_id).unwrap();
    let prose = fragments
        .iter()
        .find(|f| f.text.contains("single owner"))
        .expect("the prose fragment");
    *rig.model.fragment.lock().unwrap() = prose.fragment_id.clone();
    source.source_id
}

fn new_course(rig: &Rig, source: &str) -> String {
    rig.service
        .create_course(&NewCourse {
            title: "Rust ownership".into(),
            domain: Domain::Rust,
            level: "Beginner".into(),
            source_ids: vec![source.into()],
        })
        .unwrap()
        .course_id
}

#[tokio::test]
async fn a_book_becomes_a_published_served_lesson() {
    let rig = rig();
    let source = import_book(&rig);
    let course = new_course(&rig, &source);

    // Mapping and planning run in the background and stop for the owner.
    drain(&rig).await;
    let detail = rig.service.course(&course).unwrap();
    assert_eq!(
        detail.course.status,
        ProjectStatus::AwaitingPlanApproval,
        "{detail:?}"
    );
    let graph = detail.graph.expect("a concept graph");
    assert_eq!(graph.concepts[0].id, CONCEPT);
    assert_eq!(
        graph.built_from[0].content_hash,
        rig.service
            .sources()
            .get(&source)
            .unwrap()
            .unwrap()
            .content_hash,
        "graphs record source hashes"
    );
    let plan = detail.plan.expect("a proposed plan");
    assert!(
        rig.engine.course(&course).is_err(),
        "nothing is served before approval"
    );

    // Approval starts lesson generation; the lesson is checked and published.
    rig.service.approve_plan(&course, plan.revision).unwrap();
    drain(&rig).await;
    let detail = rig.service.course(&course).unwrap();
    assert_eq!(detail.lessons.len(), 1);
    let row = &detail.lessons[0];
    let draft_id = row.draft_id.clone().expect("a draft");
    let inspector = rig.service.draft(&draft_id).unwrap();
    assert_eq!(
        row.stage,
        Some(LessonStage::Published),
        "receipts: {:#?}",
        inspector.receipts
    );
    assert_eq!(detail.course.status, ProjectStatus::Complete);
    assert!(inspector.receipts.len() >= 6);
    assert_eq!(inspector.citations.len(), 1);

    // The engine serves it under a course-scoped id, like any embedded course.
    let served = rig.engine.course(&course).expect("served course");
    let served = serde_json::to_string(&served).unwrap();
    assert!(
        served.contains(&published_lesson_id(&course, "lesson.1")),
        "{served}"
    );
    assert!(
        !served.contains("model-chose-this"),
        "the model's own lesson id is replaced"
    );

    // Asking again changes nothing and calls no model: every step checks what's stored.
    let calls = rig.model.calls.load(Ordering::SeqCst);
    assert_eq!(calls, 3, "one concept batch, one plan, one lesson");
    rig.service.approve_plan(&course, plan.revision).unwrap();
    assert!(
        rig.service.next_lesson(&course).is_err(),
        "nothing is left to write"
    );
    drain(&rig).await;
    assert_eq!(rig.model.calls.load(Ordering::SeqCst), calls);
    assert_eq!(rig.store.drafts_for(&course).unwrap().len(), 1);
}

#[tokio::test]
async fn a_garbled_lesson_is_retried_and_paused_courses_wait() {
    let rig = rig();
    rig.model.bad_lessons.store(1, Ordering::SeqCst);
    let source = import_book(&rig);
    let course = new_course(&rig, &source);
    drain(&rig).await;
    let plan = rig.service.course(&course).unwrap().plan.unwrap();
    rig.service.approve_plan(&course, plan.revision).unwrap();

    // Paused: the lesson job waits and no model is called.
    rig.service.set_course_paused(&course, true).unwrap();
    let before = rig.model.calls.load(Ordering::SeqCst);
    let cancel = CancellationToken::new();
    assert!(matches!(rig.worker.tick(&cancel).await, Tick::Idle(_)));
    assert_eq!(rig.model.calls.load(Ordering::SeqCst), before);
    rig.service.set_course_paused(&course, false).unwrap();
    assert_eq!(
        rig.service.course(&course).unwrap().course.status,
        ProjectStatus::Generating
    );

    // The first lesson reply is prose, not JSON: a retryable failure, then a good draft.
    drain(&rig).await;
    let detail = rig.service.course(&course).unwrap();
    assert_eq!(detail.lessons[0].stage, Some(LessonStage::Published));
    assert_eq!(rig.model.calls.load(Ordering::SeqCst), before + 2);
}

#[tokio::test]
async fn bad_course_requests_are_refused() {
    let rig = rig();
    let source = import_book(&rig);
    let bad = |title: &str, sources: Vec<String>| {
        rig.service.create_course(&NewCourse {
            title: title.into(),
            domain: Domain::Rust,
            level: String::new(),
            source_ids: sources,
        })
    };
    assert!(bad("", vec![source.clone()]).is_err());
    assert!(bad("No sources", vec![]).is_err());
    assert!(bad("Unknown", vec!["nope".into()]).is_err());
    assert!(bad("Twice", vec![source.clone(), source.clone()]).is_err());
    let course = new_course(&rig, &source);
    assert!(rig.service.approve_plan(&course, 1).is_err(), "no plan yet");
    assert!(rig.service.next_lesson(&course).is_err());
}
