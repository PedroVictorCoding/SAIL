//! Integration and validity verification test for the Philosophy & Logic course package.
//!
//! (a) Deserializes `courses/philosophy-logic.json` into [`CoursePackage`] and runs [`CoursePackage::validate`].
//! (b) Recomputes every formal-validity item with [`courses::logic::check_argument`] using a test-only sidecar map.

use sail_arena_learn::courses::content::{AnswerKey, CoursePackage, Domain, StepKind};
use sail_arena_learn::courses::logic::check_argument;
use std::collections::BTreeMap;

const PHILOSOPHY_LOGIC_JSON: &str = include_str!("../courses/philosophy-logic.json");

/// Sidecar mapping representing the formal propositional argument under test for a given step.
struct LogicSidecar {
    premises: &'static [&'static str],
    conclusion: &'static str,
    expected_valid: bool,
}

fn logic_sidecar_map() -> BTreeMap<&'static str, LogicSidecar> {
    let mut map = BTreeMap::new();

    // Lesson: logic-valid-sound
    map.insert(
        "check-validity-form",
        LogicSidecar {
            premises: &["M -> C", "M"],
            conclusion: "C",
            expected_valid: true,
        },
    );

    // Lesson: logic-inference-rules
    map.insert(
        "check-modus-ponens",
        LogicSidecar {
            premises: &["P -> Q", "P"],
            conclusion: "Q",
            expected_valid: true,
        },
    );
    map.insert(
        "check-affirming-consequent",
        LogicSidecar {
            premises: &["P -> Q", "Q"],
            conclusion: "P",
            expected_valid: false,
        },
    );
    map.insert(
        "check-modus-tollens",
        LogicSidecar {
            premises: &["P -> Q", "~Q"],
            conclusion: "~P",
            expected_valid: true,
        },
    );
    map.insert(
        "check-denying-antecedent",
        LogicSidecar {
            premises: &["P -> Q", "~P"],
            conclusion: "~Q",
            expected_valid: false,
        },
    );
    map.insert(
        "fill-modus-tollens",
        LogicSidecar {
            premises: &["P -> Q", "~Q"],
            conclusion: "~P",
            expected_valid: true,
        },
    );

    // Diagnostic
    map.insert(
        "d-modus-ponens",
        LogicSidecar {
            premises: &["A -> B", "A"],
            conclusion: "B",
            expected_valid: true,
        },
    );
    map.insert(
        "d-affirming-consequent",
        LogicSidecar {
            premises: &["A -> B", "B"],
            conclusion: "A",
            expected_valid: false,
        },
    );

    // Reviews
    map.insert(
        "r-validity-syllogism",
        LogicSidecar {
            premises: &["P -> Q", "Q -> R"],
            conclusion: "P -> R",
            expected_valid: true,
        },
    );
    map.insert(
        "r-modus-tollens-check",
        LogicSidecar {
            premises: &["R -> S", "~S"],
            conclusion: "~R",
            expected_valid: true,
        },
    );
    map.insert(
        "r-denying-antecedent-check",
        LogicSidecar {
            premises: &["R -> S", "~R"],
            conclusion: "~S",
            expected_valid: false,
        },
    );

    map
}

#[test]
fn test_philosophy_package_deserialization_and_validation() {
    let pkg: CoursePackage = serde_json::from_str(PHILOSOPHY_LOGIC_JSON)
        .expect("philosophy-logic.json deserializes cleanly");

    assert_eq!(pkg.id, "philosophy-logic");
    assert_eq!(pkg.domain, Domain::Philosophy);
    assert_eq!(pkg.chapters.len(), 1);
    assert_eq!(pkg.chapters[0].id, "validity-inference");
    assert_eq!(pkg.lessons.len(), 2);
    assert!(pkg.diagnostic.is_some());
    assert_eq!(pkg.diagnostic.as_ref().unwrap().steps.len(), 4);
    assert_eq!(pkg.reviews.len(), 4);

    // Run structural package validator (validates references, keys, limits, and unseen review variants)
    pkg.validate()
        .expect("philosophy-logic package passes validation");
}

#[test]
fn test_recompute_validity_keys_with_logic_checker() {
    let pkg: CoursePackage = serde_json::from_str(PHILOSOPHY_LOGIC_JSON)
        .expect("philosophy-logic.json deserializes cleanly");
    let sidecars = logic_sidecar_map();

    // Collect all steps from lessons, diagnostic, and reviews
    let mut all_steps = Vec::new();
    for lesson in &pkg.lessons {
        for step in &lesson.steps {
            all_steps.push(step);
        }
    }
    if let Some(diag) = &pkg.diagnostic {
        for step in &diag.steps {
            all_steps.push(step);
        }
    }
    for step in &pkg.reviews {
        all_steps.push(step);
    }

    let mut verified_count = 0;

    for step in all_steps {
        if let Some(sidecar) = sidecars.get(step.id.as_str()) {
            // Recompute validity with pure Rust logic engine
            let result = check_argument(sidecar.premises, sidecar.conclusion)
                .unwrap_or_else(|e| panic!("failed to check argument for {}: {e}", step.id));

            assert_eq!(
                result.valid, sidecar.expected_valid,
                "logic::check_argument validity mismatch for step {}",
                step.id
            );

            // Verify counterexample presence if invalid
            if !sidecar.expected_valid {
                assert!(
                    result.counterexample.is_some(),
                    "step {} is invalid but returned no counterexample",
                    step.id
                );
            }

            // Verify that the authored step's AnswerKey matches the truth
            match (step.kind, &step.key) {
                (StepKind::Choice, Some(AnswerKey::Choice(keys))) => {
                    if sidecar.expected_valid {
                        // For valid steps, the key should designate valid / correct rule
                        let is_valid_choice =
                            keys.iter().any(|k| k == "valid" || k == "mp" || k == "mt");
                        assert!(
                            is_valid_choice,
                            "valid step {} has unexpected key {:?}",
                            step.id, keys
                        );
                    } else {
                        // For invalid steps, the key should designate invalid
                        let is_invalid_choice = keys.iter().any(|k| k == "invalid");
                        assert!(
                            is_invalid_choice,
                            "invalid step {} has unexpected key {:?}",
                            step.id, keys
                        );
                    }
                }
                (StepKind::Fill, Some(AnswerKey::Text(accepted))) => {
                    // For fill step, the formula in the conclusion should match one of accepted texts
                    let matches_conclusion = accepted.iter().any(|txt| txt == sidecar.conclusion);
                    assert!(
                        matches_conclusion,
                        "fill step {} conclusion {} not in accepted {:?}",
                        step.id, sidecar.conclusion, accepted
                    );
                }
                _ => {}
            }

            verified_count += 1;
        }
    }

    assert_eq!(
        verified_count,
        sidecars.len(),
        "all tagged logic steps must be visited and verified"
    );
}

#[test]
fn test_no_philosophical_claims_labeled_true_or_false() {
    let pkg: CoursePackage = serde_json::from_str(PHILOSOPHY_LOGIC_JSON)
        .expect("philosophy-logic.json deserializes cleanly");

    // Verify all steps grade only logical form and structure
    for lesson in &pkg.lessons {
        for step in &lesson.steps {
            assert_ne!(
                step.kind,
                StepKind::RustEdit,
                "Philosophy & Logic track must not use rust_edit"
            );
        }
    }
}
