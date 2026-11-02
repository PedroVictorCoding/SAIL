use sail_arena::MemoryStore;
use sail_arena::knowledge::*;

const T0: i64 = 1_760_000_000_000;
const HOUR: i64 = 3_600_000;

fn agent() -> Actor {
    Actor::subsystem("agents")
}

fn entity(tx: &mut KnowledgeTx<'_>, kind: EntityKind, name: &str, scope: &str) -> i64 {
    tx.create_entity(&NewEntity {
        kind,
        name,
        aliases: &[],
        scope,
        sensitivity: Sensitivity::Normal,
    })
    .unwrap()
}

fn evidence(
    tx: &mut KnowledgeTx<'_>,
    kind: SourceKind,
    content: &str,
    at: i64,
    scope: &str,
) -> i64 {
    tx.add_evidence(&NewEvidence {
        source_kind: kind,
        source_ref: None,
        observed_at: at,
        content,
        receipt: Some(content),
        derived_from: None,
        verification: None,
        scope,
    })
    .unwrap()
}

fn fact<'a>(
    subject: i64,
    predicate: &'a str,
    value: &str,
    status: EpistemicStatus,
    at: i64,
    scope: &'a str,
) -> NewClaim<'a> {
    NewClaim {
        subject,
        predicate,
        object: ClaimObject::Value(value.into()),
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
    }
}

fn temp_db() -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("sail-knowledge-{}", uuid::Uuid::new_v4()));
    let path = dir.join("arena.db");
    (dir, path)
}

/// Phase 1 gate: claims and relationships survive a restart with referential integrity.
#[test]
fn claims_and_relationships_survive_restart() {
    let (dir, path) = temp_db();
    let (sail, claim, edge) = {
        let store = MemoryStore::open(&path).unwrap();
        store
            .knowledge()
            .write(agent(), |tx| {
                let sail = entity(tx, EntityKind::Project, "SAIL", "project:sail");
                let rust = entity(tx, EntityKind::Concept, "Rust", "project:sail");
                let ev = evidence(
                    tx,
                    SourceKind::ExternalArtifact,
                    "Cargo.toml workspace",
                    T0,
                    "project:sail",
                );
                let claim = tx
                    .assert_claim(
                        &fact(
                            sail,
                            "operating_environment",
                            "Ubuntu Linux",
                            EpistemicStatus::Verified,
                            T0,
                            "project:sail",
                        ),
                        &[(ev, Stance::Supports)],
                    )
                    .unwrap();
                let edge = tx
                    .assert_claim(
                        &NewClaim {
                            object: ClaimObject::Entity(rust),
                            ..fact(
                                sail,
                                "primary_language",
                                "-",
                                EpistemicStatus::Verified,
                                T0,
                                "project:sail",
                            )
                        },
                        &[(ev, Stance::Supports)],
                    )
                    .unwrap();
                Ok((sail, claim, edge))
            })
            .unwrap()
    };
    let store = MemoryStore::open(&path).unwrap();
    let kn = store.knowledge();
    let owner = Access::owner();
    let current = kn
        .current(sail, "operating_environment", &owner, T0 + 1)
        .unwrap()
        .unwrap();
    assert_eq!(current.id, claim);
    assert_eq!(current.object, ClaimObject::Value("Ubuntu Linux".into()));
    assert!(current.last_verified_at.is_some());
    let edges = kn.neighbors(sail, &owner, false).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(
        (
            edges[0].claim_id,
            edges[0].relation.as_str(),
            edges[0].other.name.as_str()
        ),
        (edge, "primary_language", "Rust")
    );
    assert_eq!(
        kn.evidence(claim, &owner).unwrap()[0]
            .evidence
            .receipt
            .as_deref(),
        Some("Cargo.toml workspace")
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn referential_integrity_is_enforced_and_failed_transactions_roll_back() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let err = kn
        .write(agent(), |tx| {
            let a = entity(tx, EntityKind::Concept, "orphan check", "user");
            let ev = evidence(tx, SourceKind::UserStatement, "x", T0, "user");
            tx.assert_claim(
                &NewClaim {
                    object: ClaimObject::Entity(9_999),
                    ..fact(
                        a,
                        "relates_to",
                        "-",
                        EpistemicStatus::UserReported,
                        T0,
                        "user",
                    )
                },
                &[(ev, Stance::Supports)],
            )
        })
        .unwrap_err();
    assert_eq!(err, KnowledgeError::NotFound("entity", 9_999));
    assert!(
        kn.find_entities("orphan check", None, &Access::owner())
            .unwrap()
            .is_empty(),
        "rolled back"
    );
    assert_eq!(
        kn.operations(10).unwrap().len(),
        0,
        "the log rolls back with the change"
    );
}

#[test]
fn status_must_be_supported_by_evidence() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let (env, said) = kn
        .write(agent(), |tx| {
            let env = entity(tx, EntityKind::Environment, "SAIL host", "system");
            let said = evidence(
                tx,
                SourceKind::UserStatement,
                "the root disk is full",
                T0,
                "system",
            );
            Ok((env, said))
        })
        .unwrap();
    let verify = kn.write(agent(), |tx| {
        tx.assert_claim(
            &fact(
                env,
                "root_usage",
                "100%",
                EpistemicStatus::Verified,
                T0,
                "system",
            ),
            &[(said, Stance::Supports)],
        )
    });
    assert!(
        matches!(verify, Err(KnowledgeError::InsufficientEvidence(_))),
        "{verify:?}"
    );
    let no_evidence = kn.write(agent(), |tx| {
        tx.assert_claim(
            &fact(
                env,
                "root_usage",
                "100%",
                EpistemicStatus::Unverified,
                T0,
                "system",
            ),
            &[],
        )
    });
    assert!(matches!(
        no_evidence,
        Err(KnowledgeError::InsufficientEvidence(_))
    ));
    let confident_fact = kn.write(agent(), |tx| {
        tx.assert_claim(
            &NewClaim {
                inference_confidence: Some(0.9),
                ..fact(
                    env,
                    "root_usage",
                    "100%",
                    EpistemicStatus::UserReported,
                    T0,
                    "system",
                )
            },
            &[(said, Stance::Supports)],
        )
    });
    assert!(matches!(confident_fact, Err(KnowledgeError::Invalid(_))));

    let claim = kn
        .write(agent(), |tx| {
            tx.assert_claim(
                &fact(
                    env,
                    "root_usage",
                    "100%",
                    EpistemicStatus::UserReported,
                    T0,
                    "system",
                ),
                &[(said, Stance::Supports)],
            )
        })
        .unwrap();
    let upgrade = kn.write(agent(), |tx| {
        tx.set_status(claim, EpistemicStatus::Verified, None)
    });
    assert!(matches!(
        upgrade,
        Err(KnowledgeError::InsufficientEvidence(_))
    ));
    kn.write(agent(), |tx| {
        let df = evidence(
            tx,
            SourceKind::ToolObservation,
            "df: / 100%",
            T0 + 1,
            "system",
        );
        tx.link_evidence(claim, df, Stance::Supports)?;
        tx.set_status(claim, EpistemicStatus::Verified, Some("df output"))
    })
    .unwrap();
    assert_eq!(
        kn.current(env, "root_usage", &Access::owner(), T0 + 2)
            .unwrap()
            .unwrap()
            .status,
        EpistemicStatus::Verified
    );
}

#[test]
fn restatements_do_not_count_as_independent_support() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let owner = Access::owner();
    let (claim, first) = kn
        .write(agent(), |tx| {
            let user = entity(tx, EntityKind::Person, "Owner", "user");
            let said = evidence(
                tx,
                SourceKind::UserStatement,
                "I prefer local inference",
                T0,
                "user",
            );
            let pref = NewClaim {
                category: ClaimCategory::Preference,
                basis: Some(PreferenceBasis::Explicit),
                ..fact(
                    user,
                    "prefers",
                    "local inference",
                    EpistemicStatus::UserReported,
                    T0,
                    "user",
                )
            };
            let claim = tx.assert_claim(&pref, &[(said, Stance::Supports)])?;
            // Two summaries of that same statement, recorded later by consolidation.
            for summary in ["user likes local models", "prefers on-device inference"] {
                let restated = tx.add_evidence(&NewEvidence {
                    source_kind: SourceKind::PriorMemory,
                    source_ref: None,
                    observed_at: T0 + HOUR,
                    content: summary,
                    receipt: None,
                    derived_from: Some(said),
                    verification: None,
                    scope: "user",
                })?;
                let again = tx.assert_claim(
                    &NewClaim {
                        observed_at: T0 + HOUR,
                        ..pref.clone()
                    },
                    &[(restated, Stance::Supports)],
                )?;
                assert_eq!(
                    again, claim,
                    "an equivalent claim reinforces the existing one"
                );
            }
            // The same statement recorded twice is one piece of evidence.
            assert_eq!(
                evidence(
                    tx,
                    SourceKind::UserStatement,
                    "I prefer local inference",
                    T0,
                    "user"
                ),
                said
            );
            Ok((claim, said))
        })
        .unwrap();
    let support = kn.support(claim, &owner).unwrap();
    assert_eq!(support.user_origins, 1, "{support:?}");
    assert_eq!(kn.evidence(claim, &owner).unwrap().len(), 3);

    let missing_origin = kn.write(agent(), |tx| {
        tx.add_evidence(&NewEvidence {
            source_kind: SourceKind::PriorMemory,
            source_ref: None,
            observed_at: T0,
            content: "?",
            receipt: None,
            derived_from: None,
            verification: None,
            scope: "user",
        })
    });
    assert!(matches!(missing_origin, Err(KnowledgeError::Invalid(_))));
    let _ = first;
}

#[test]
fn current_state_follows_observation_time_not_arrival_order() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let owner = Access::owner();
    let (disk, newer, older) = kn
        .write(agent(), |tx| {
            let disk = entity(tx, EntityKind::Device, "Ubuntu root partition", "system");
            // The later reading arrives first.
            let e2 = evidence(
                tx,
                SourceKind::ToolObservation,
                "df 58%",
                T0 + 2 * HOUR,
                "system",
            );
            let newer = tx.assert_claim(
                &fact(
                    disk,
                    "usage",
                    "58%",
                    EpistemicStatus::Verified,
                    T0 + 2 * HOUR,
                    "system",
                ),
                &[(e2, Stance::Supports)],
            )?;
            let e1 = evidence(tx, SourceKind::ToolObservation, "df 100%", T0, "system");
            let older = tx.assert_claim(
                &fact(
                    disk,
                    "usage",
                    "100%",
                    EpistemicStatus::Verified,
                    T0,
                    "system",
                ),
                &[(e1, Stance::Supports)],
            )?;
            Ok((disk, newer, older))
        })
        .unwrap();
    assert_eq!(
        kn.current(disk, "usage", &owner, T0 + 3 * HOUR)
            .unwrap()
            .unwrap()
            .id,
        newer
    );

    let backwards = kn.write(agent(), |tx| tx.supersede(newer, older, "late arrival"));
    assert!(
        matches!(backwards, Err(KnowledgeError::Invalid(_))),
        "{backwards:?}"
    );
    kn.write(agent(), |tx| tx.supersede(older, newer, "newer df reading"))
        .unwrap();

    let history = kn.history(disk, "usage", &owner).unwrap();
    assert_eq!(
        history.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![older, newer]
    );
    assert_eq!(history[0].status, EpistemicStatus::Superseded);
    assert_eq!(history[0].superseded_by, Some(newer));
    assert_eq!(history[0].valid_until, Some(T0 + 2 * HOUR));
    assert_eq!(kn.claims_about(disk, &owner, false).unwrap().len(), 1);
}

#[test]
fn inferred_preferences_never_override_explicit_ones() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let (user, explicit) = kn
        .write(agent(), |tx| {
            let user = entity(tx, EntityKind::Person, "Owner", "user");
            let said = evidence(
                tx,
                SourceKind::UserStatement,
                "keep interfaces minimal",
                T0,
                "user",
            );
            let guess = evidence(
                tx,
                SourceKind::ModelInference,
                "user opened many panels",
                T0 + HOUR,
                "user",
            );
            let pref = |value: &str, basis, status, at| NewClaim {
                category: ClaimCategory::Preference,
                basis: Some(basis),
                ..fact(user, "interface_density", value, status, at, "user")
            };
            let explicit = tx.assert_claim(
                &pref(
                    "minimal",
                    PreferenceBasis::Explicit,
                    EpistemicStatus::UserReported,
                    T0,
                ),
                &[(said, Stance::Supports)],
            )?;
            tx.assert_claim(
                &NewClaim {
                    inference_confidence: Some(0.4),
                    ..pref(
                        "dense",
                        PreferenceBasis::Inferred,
                        EpistemicStatus::Inferred,
                        T0 + HOUR,
                    )
                },
                &[(guess, Stance::Supports)],
            )?;
            Ok((user, explicit))
        })
        .unwrap();
    assert_eq!(
        kn.current(user, "interface_density", &Access::owner(), T0 + 2 * HOUR)
            .unwrap()
            .unwrap()
            .id,
        explicit
    );
}

#[test]
fn project_work_needs_direct_evidence_to_be_complete() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let (sail, task) = kn
        .write(agent(), |tx| {
            let sail = entity(tx, EntityKind::Project, "SAIL", "project:sail");
            let task = tx.add_project_item(&NewProjectItem {
                project: sail,
                kind: ProjectItemKind::Task,
                title: "Move Cargo target dir to the runtime SSD",
                detail: None,
                state: WorkState::Approved,
                scope: "project:sail",
                sensitivity: Sensitivity::Normal,
            })?;
            Ok((sail, task))
        })
        .unwrap();
    let start_done = kn.write(agent(), |tx| {
        tx.add_project_item(&NewProjectItem {
            project: sail,
            kind: ProjectItemKind::Task,
            title: "x",
            detail: None,
            state: WorkState::Implemented,
            scope: "project:sail",
            sensitivity: Sensitivity::Normal,
        })
    });
    assert!(matches!(start_done, Err(KnowledgeError::Invalid(_))));
    let claimed = kn.write(agent(), |tx| {
        let said = evidence(
            tx,
            SourceKind::ModelInference,
            "I have moved the target directory.",
            T0,
            "project:sail",
        );
        tx.set_project_state(task, WorkState::Implemented, Some(said), None)
    });
    assert!(
        matches!(claimed, Err(KnowledgeError::InsufficientEvidence(_))),
        "{claimed:?}"
    );
    let bare = kn.write(agent(), |tx| {
        tx.set_project_state(task, WorkState::Tested, None, None)
    });
    assert!(matches!(bare, Err(KnowledgeError::InsufficientEvidence(_))));
    kn.write(agent(), |tx| {
        let observed = evidence(
            tx,
            SourceKind::ToolObservation,
            "cargo metadata: target_directory=/srv/sail-data/target",
            T0,
            "project:sail",
        );
        tx.set_project_state(
            task,
            WorkState::Implemented,
            Some(observed),
            Some("verified config"),
        )
    })
    .unwrap();
    let items = kn.project_items(sail, &Access::owner()).unwrap();
    assert_eq!(items[0].state, WorkState::Implemented);
    assert!(items[0].state_evidence.is_some());
}

#[test]
fn procedures_are_versioned_records_not_commands() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let owner = Access::owner();
    let spec = ProcedureSpec {
        summary: "Safely relocate Cargo build artifacts".into(),
        preconditions: vec![
            "Destination filesystem is mounted".into(),
            "Active builds are stopped".into(),
        ],
        required_capabilities: vec!["fs:write:/srv/sail-data".into()],
        steps: vec![ProcedureStep {
            description: "Configure Cargo target directory".into(),
            workflow_ref: Some("forge:cargo_target_dir".into()),
        }],
        postconditions: vec!["Builds use the new destination".into()],
        ..Default::default()
    };
    let (proc_entity, v1, v2) = kn
        .write(agent(), |tx| {
            let p = entity(
                tx,
                EntityKind::Procedure,
                "Relocate Cargo artifacts",
                "project:sail",
            );
            let (v1, n1) = tx.put_procedure(p, &spec)?;
            let (v2, n2) = tx.put_procedure(
                p,
                &ProcedureSpec {
                    summary: "v2".into(),
                    ..spec.clone()
                },
            )?;
            assert_eq!((n1, n2), (1, 2));
            Ok((p, v1, v2))
        })
        .unwrap();
    let shell = kn.write(agent(), |tx| {
        tx.put_procedure(
            proc_entity,
            &ProcedureSpec {
                steps: vec![ProcedureStep {
                    description: "wipe".into(),
                    workflow_ref: Some("rm -rf ~/.cargo".into()),
                }],
                ..spec.clone()
            },
        )
    });
    assert!(matches!(shell, Err(KnowledgeError::Invalid(_))));
    let claimed_run = kn.write(agent(), |tx| {
        let said = evidence(
            tx,
            SourceKind::UserStatement,
            "it worked",
            T0,
            "project:sail",
        );
        tx.record_procedure_run(v1, RunOutcome::Succeeded, said, None)
    });
    assert!(matches!(
        claimed_run,
        Err(KnowledgeError::InsufficientEvidence(_))
    ));
    let versions = kn.procedure_versions(proc_entity, &owner).unwrap();
    assert_eq!(
        versions.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![v2, v1]
    );
    assert!(versions.iter().all(|p| p.status == ProcedureStatus::Draft));
    assert_eq!(versions[1].spec, spec);
}

#[test]
fn scopes_isolate_projects_life_and_graph_traversal() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let (owner_entity, sail, secret) = kn
        .write(agent(), |tx| {
            let me = entity(tx, EntityKind::Person, "Owner", "user");
            let sail = entity(tx, EntityKind::Project, "SAIL", "project:sail");
            let clinic = entity(tx, EntityKind::Organization, "Clinic", "life");
            let e_sail = evidence(
                tx,
                SourceKind::UserStatement,
                "I build SAIL",
                T0,
                "project:sail",
            );
            let e_life = evidence(
                tx,
                SourceKind::UserStatement,
                "appointment at the clinic",
                T0,
                "life",
            );
            tx.assert_claim(
                &NewClaim {
                    object: ClaimObject::Entity(sail),
                    ..fact(
                        me,
                        "works_on",
                        "-",
                        EpistemicStatus::UserReported,
                        T0,
                        "project:sail",
                    )
                },
                &[(e_sail, Stance::Supports)],
            )?;
            tx.assert_claim(
                &NewClaim {
                    object: ClaimObject::Entity(clinic),
                    ..fact(
                        me,
                        "patient_of",
                        "-",
                        EpistemicStatus::UserReported,
                        T0,
                        "life",
                    )
                },
                &[(e_life, Stance::Supports)],
            )?;
            let secret = tx.assert_claim(
                &NewClaim {
                    sensitivity: Sensitivity::Sensitive,
                    ..fact(
                        sail,
                        "signing_key_location",
                        "~/.keys",
                        EpistemicStatus::UserReported,
                        T0,
                        "project:sail",
                    )
                },
                &[(e_sail, Stance::Supports)],
            )?;
            Ok((me, sail, secret))
        })
        .unwrap();
    // Forge sees the SAIL project and the owner entity, nothing from Life, nothing sensitive.
    let forge = Access::scoped(&["project:sail", "user"], Sensitivity::Private);
    let edges = kn.neighbors(owner_entity, &forge, false).unwrap();
    assert_eq!(
        edges
            .iter()
            .map(|e| e.other.name.as_str())
            .collect::<Vec<_>>(),
        vec!["SAIL"]
    );
    assert!(kn.find_entities("clinic", None, &forge).unwrap().is_empty());
    assert!(kn.claims_about(sail, &forge, false).unwrap().is_empty());
    assert_eq!(
        kn.evidence(secret, &forge).unwrap_err(),
        KnowledgeError::NotFound("claim", secret)
    );
    // A caller without the subject's scope cannot walk from it at all.
    let browser = Access::scoped(&["web"], Sensitivity::Normal);
    assert!(
        kn.neighbors(owner_entity, &browser, false)
            .unwrap()
            .is_empty()
    );
    assert!(kn.entity(sail, &browser).unwrap().is_none());
    assert_eq!(
        kn.neighbors(owner_entity, &Access::owner(), false)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn deleted_claims_cannot_be_recreated_by_stale_jobs() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let owner = Access::owner();
    let (user, claim) = kn
        .write(agent(), |tx| {
            let user = entity(tx, EntityKind::Person, "Owner", "user");
            let ev = evidence(
                tx,
                SourceKind::UserStatement,
                "my bank is Caixa",
                T0,
                "user",
            );
            let claim = tx.assert_claim(
                &fact(
                    user,
                    "bank",
                    "Caixa",
                    EpistemicStatus::UserReported,
                    T0,
                    "user",
                ),
                &[(ev, Stance::Supports)],
            )?;
            Ok((user, claim))
        })
        .unwrap();
    // A background job starts and snapshots the generation...
    let job_generation = kn.deletion_generation().unwrap();
    // ...then the user forgets the claim.
    kn.write(Actor::User, |tx| {
        tx.delete_claim(claim, "user asked to forget")
    })
    .unwrap();
    assert!(kn.claims_about(user, &owner, true).unwrap().is_empty());
    let receipts = kn.operations(10).unwrap();
    assert!(
        !format!("{receipts:?}").contains("Caixa"),
        "the change log keeps no deleted content"
    );

    let stale = kn.write_at_generation(job_generation, agent(), |tx| {
        let ev = evidence(tx, SourceKind::UserStatement, "unrelated", T0, "user");
        tx.assert_claim(
            &fact(
                user,
                "city",
                "Lisbon",
                EpistemicStatus::UserReported,
                T0,
                "user",
            ),
            &[(ev, Stance::Supports)],
        )
    });
    assert!(
        matches!(stale, Err(KnowledgeError::StaleGeneration { .. })),
        "{stale:?}"
    );

    // A fresh job replaying the old transcript is stopped by the tombstone.
    let replay = kn.write(agent(), |tx| {
        let ev = evidence(
            tx,
            SourceKind::UserStatement,
            "my bank is Caixa",
            T0,
            "user",
        );
        tx.assert_claim(
            &fact(
                user,
                "bank",
                "caixa ",
                EpistemicStatus::UserReported,
                T0,
                "user",
            ),
            &[(ev, Stance::Supports)],
        )
    });
    assert_eq!(replay.unwrap_err(), KnowledgeError::Tombstoned);
    // The user can teach it again on purpose.
    kn.write(Actor::User, |tx| {
        let ev = evidence(
            tx,
            SourceKind::UserStatement,
            "my bank is Caixa",
            T0 + HOUR,
            "user",
        );
        tx.assert_claim(
            &fact(
                user,
                "bank",
                "Caixa",
                EpistemicStatus::UserReported,
                T0 + HOUR,
                "user",
            ),
            &[(ev, Stance::Supports)],
        )
    })
    .unwrap();
}

#[test]
fn deleting_an_entity_removes_its_claims_evidence_and_merged_aliases() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kn = store.knowledge();
    let owner = Access::owner();
    let (alex, other_alex, keep_ev) = kn
        .write(agent(), |tx| {
            let alex = entity(tx, EntityKind::Person, "Alex", "user");
            let other = entity(tx, EntityKind::Person, "Alex", "user");
            let dup = tx.create_entity(&NewEntity {
                kind: EntityKind::Person,
                name: "Alexandre",
                aliases: &["Alex M."],
                scope: "user",
                sensitivity: Sensitivity::Normal,
            })?;
            let ev = evidence(
                tx,
                SourceKind::UserStatement,
                "Alex plays cello",
                T0,
                "user",
            );
            tx.assert_claim(
                &fact(
                    dup,
                    "plays",
                    "cello",
                    EpistemicStatus::UserReported,
                    T0,
                    "user",
                ),
                &[(ev, Stance::Supports)],
            )?;
            let keep = evidence(
                tx,
                SourceKind::UserStatement,
                "the other Alex runs",
                T0,
                "user",
            );
            tx.assert_claim(
                &fact(
                    other,
                    "hobby",
                    "running",
                    EpistemicStatus::UserReported,
                    T0,
                    "user",
                ),
                &[(keep, Stance::Supports)],
            )?;
            tx.merge_entities(dup, alex, "user confirmed same person")?;
            Ok((alex, other, keep))
        })
        .unwrap();
    // Homonyms stay distinct; the merged name resolves to its canonical entity.
    assert_eq!(
        kn.find_entities("alex", Some(EntityKind::Person), &owner)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        kn.find_entities("Alex M.", None, &owner).unwrap()[0].id,
        alex
    );
    assert_eq!(
        kn.claims_about(alex, &owner, false).unwrap()[0].predicate,
        "plays"
    );

    kn.write(Actor::User, |tx| {
        tx.delete_entity(alex, "forget this person")
    })
    .unwrap();
    assert!(
        kn.find_entities("Alexandre", None, &owner)
            .unwrap()
            .is_empty()
    );
    assert!(kn.entity(alex, &owner).unwrap().is_none());
    let remaining = kn.claims_about(other_alex, &owner, false).unwrap();
    assert_eq!(remaining.len(), 1, "the homonym is untouched");
    assert_eq!(
        kn.evidence(remaining[0].id, &owner).unwrap()[0].evidence.id,
        keep_ev
    );
    // A replayed transcript may recreate the name, but not the forgotten knowledge.
    let recreate = kn.write(agent(), |tx| {
        let again = tx.create_entity(&NewEntity {
            kind: EntityKind::Person,
            name: "Alexandre",
            aliases: &[],
            scope: "user",
            sensitivity: Sensitivity::Normal,
        })?;
        let ev = evidence(
            tx,
            SourceKind::UserStatement,
            "Alex plays cello",
            T0,
            "user",
        );
        tx.assert_claim(
            &fact(
                again,
                "plays",
                "cello",
                EpistemicStatus::UserReported,
                T0,
                "user",
            ),
            &[(ev, Stance::Supports)],
        )
    });
    assert_eq!(recreate.unwrap_err(), KnowledgeError::Tombstoned);
    assert!(
        kn.find_entities("Alexandre", None, &owner)
            .unwrap()
            .is_empty(),
        "rolled back with the claim"
    );
    assert_eq!(store.check_integrity().unwrap(), Vec::<String>::new());
}
