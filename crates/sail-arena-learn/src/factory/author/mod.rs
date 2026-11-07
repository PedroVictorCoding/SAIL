//! Course Factory authoring: bounded retrieval, context, prompts, reply parsing and assembly.
//!
//! This module owns the model-facing half of the pipeline:
//! - [`retrieve`] ranks source fragments lexically, deterministically and without embeddings;
//! - [`context`] builds the one bounded prompt a call may see;
//! - [`prompts`] holds the four original prompts and their reply shapes;
//! - [`parse`] turns a reply into a contract value and hashes exactly what was stored;
//! - [`assemble`] merges proposals into the server-owned graph and plan.
//!
//! The call functions below are thin on purpose. They build the prompt, call
//! [`AuthorModel`] once, and parse the reply. They never retry: retrying, backoff and
//! admission belong to the scheduler, and an [`AuthorModelError`] is passed through wrapped
//! but otherwise unchanged so the scheduler can distinguish waiting from failed.
//!
//! Nothing here writes storage or reads a clock. The caller supplies fragments, the learner
//! evidence summary and the cancellation token.

pub mod assemble;
pub mod context;
pub mod parse;
pub mod prompts;
pub mod retrieve;

use crate::courses::content::Step;
use crate::factory::author::context::BuiltContext;
use crate::factory::author::parse::Parsed;
use crate::factory::contracts::{
    AuthorModel, AuthorModelError, ConceptGraph, ConceptProposal, CurriculumPlan,
    CurriculumProposal, LessonDraftOutput, ObjectiveSpec, SourceFragment,
};
use tokio_util::sync::CancellationToken;

pub use assemble::{
    AssembleError, AssemblyReport, ConceptBatch, GraphEdit, apply_owner_edits,
    assemble_concept_graph, assemble_curriculum_plan,
};
pub use context::{
    AuthoringContext, BuiltContext as BuiltAuthoringContext, ContextBudget, ContextError,
};
pub use parse::{ParseError, Parsed as ParsedReply};
pub use retrieve::{FragmentIndex, RetrievalCache, RetrievedFragment};

/// Why an authoring call produced no usable value.
#[derive(Debug, thiserror::Error)]
pub enum AuthorError {
    /// The model could not serve the call. Unchanged, so the scheduler can map `Unavailable`
    /// to waiting, `Preempted` to requeue and `Failed` to a bounded retry.
    #[error(transparent)]
    Model(#[from] AuthorModelError),
    /// The model answered but the answer is not a valid contract value.
    #[error(transparent)]
    Parse(#[from] ParseError),
    /// The caller asked for a lesson that is not in the plan, or built a context for the
    /// wrong objective.
    #[error("invalid authoring request: {0}")]
    InvalidRequest(String),
}

/// One concept-extraction call's input: the course and the fragments in this bounded batch.
pub struct ConceptBatchInput<'a> {
    pub course_id: &'a str,
    pub fragments: &'a [SourceFragment],
}

/// Asks the model for concepts and objectives supported by one bounded batch of fragments.
pub async fn extract_concepts(
    model: &dyn AuthorModel,
    batch: &ConceptBatchInput<'_>,
    cancel: CancellationToken,
) -> Result<Parsed<ConceptProposal>, AuthorError> {
    let prompt = prompts::concept_extraction(batch.course_id, batch.fragments);
    let reply = model.complete(&prompt, cancel).await?;
    Ok(parse::parse_concept_proposal(&reply)?)
}

/// Asks the model to plan units and lessons from an assembled concept graph.
pub async fn plan_curriculum(
    model: &dyn AuthorModel,
    graph: &ConceptGraph,
    cancel: CancellationToken,
) -> Result<Parsed<CurriculumProposal>, AuthorError> {
    let prompt = prompts::curriculum_planning(graph);
    let reply = model.complete(&prompt, cancel).await?;
    Ok(parse::parse_curriculum_proposal(&reply)?)
}

/// Asks the model for one lesson draft.
///
/// `lesson_plan_id` must exist in `plan`; that is a caller error, so the model is not called.
pub async fn author_lesson(
    model: &dyn AuthorModel,
    plan: &CurriculumPlan,
    lesson_plan_id: &str,
    ctx: &BuiltContext,
    cancel: CancellationToken,
) -> Result<Parsed<LessonDraftOutput>, AuthorError> {
    let known = plan
        .units
        .iter()
        .flat_map(|unit| unit.lessons.iter())
        .find(|lesson| lesson.id == lesson_plan_id);
    let Some(lesson) = known else {
        return Err(AuthorError::InvalidRequest(format!(
            "lesson `{lesson_plan_id}` is not in planning revision {}",
            plan.revision
        )));
    };
    if !lesson.objective_ids.contains(&ctx.objective_id) {
        return Err(AuthorError::InvalidRequest(format!(
            "lesson `{lesson_plan_id}` does not teach objective `{}`",
            ctx.objective_id
        )));
    }
    if ctx.schema != prompts::SCHEMA_LESSON {
        return Err(AuthorError::InvalidRequest(format!(
            "lesson context has schema `{}`, expected `{}`",
            ctx.schema,
            prompts::SCHEMA_LESSON
        )));
    }
    let prompt = prompts::lesson_authoring(ctx);
    let reply = model.complete(&prompt, cancel).await?;
    Ok(parse::parse_lesson_draft(&reply)?)
}

/// Asks the model for one unseen review variant of an objective.
pub async fn author_review_variant(
    model: &dyn AuthorModel,
    objective: &ObjectiveSpec,
    ctx: &BuiltContext,
    cancel: CancellationToken,
) -> Result<Parsed<Step>, AuthorError> {
    if ctx.schema != prompts::SCHEMA_REVIEW {
        return Err(AuthorError::InvalidRequest(format!(
            "review variant context has schema `{}`, expected `{}`",
            ctx.schema,
            prompts::SCHEMA_REVIEW
        )));
    }
    if ctx.objective_id != objective.id {
        return Err(AuthorError::InvalidRequest(format!(
            "context was built for `{}`, not `{}`",
            ctx.objective_id, objective.id
        )));
    }
    let prompt = prompts::review_variant(ctx);
    let reply = model.complete(&prompt, cancel).await?;
    Ok(parse::parse_review_variant(&reply)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::author::context::AuthoringContext;
    use crate::factory::contracts::{
        AuthorPrompt, FragmentKind, QualityStatus, SourceFragment, SourceLocation,
    };
    use std::sync::Mutex;

    /// A model that returns scripted replies in order and records the prompts it saw.
    struct ScriptedModel {
        replies: Mutex<Vec<Result<String, AuthorModelError>>>,
        prompts: Mutex<Vec<AuthorPrompt>>,
    }

    impl ScriptedModel {
        fn new(replies: Vec<Result<String, AuthorModelError>>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().rev().collect()),
                prompts: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.prompts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
        }
    }

    #[async_trait::async_trait]
    impl AuthorModel for ScriptedModel {
        async fn complete(
            &self,
            prompt: &AuthorPrompt,
            _cancel: CancellationToken,
        ) -> Result<String, AuthorModelError> {
            self.prompts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(prompt.clone());
            self.replies
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop()
                .unwrap_or_else(|| {
                    Err(AuthorModelError::Unavailable("script exhausted".to_owned()))
                })
        }
    }

    fn fragment(id: &str, text: &str) -> SourceFragment {
        SourceFragment {
            fragment_id: id.to_owned(),
            source_id: "src-rust".to_owned(),
            source_revision: 1,
            kind: FragmentKind::Prose,
            chapter_path: vec!["Ownership".to_owned()],
            location: SourceLocation::Lines { first: 1, last: 2 },
            text: text.to_owned(),
            content_hash: format!("hash-{id}"),
            quality: QualityStatus::Clean,
        }
    }

    fn objective() -> ObjectiveSpec {
        ObjectiveSpec {
            id: "rust.ownership".to_owned(),
            statement: "The learner can explain a move.".to_owned(),
        }
    }

    fn review_context() -> BuiltContext {
        let objective = objective();
        context::build(
            &AuthoringContext {
                objective: &objective,
                prerequisites: &[],
                fragments: &[fragment("f-1", "A move transfers ownership.")],
                related_lessons: &[],
                question_families: &[],
                learner_evidence: "",
                schema: prompts::SCHEMA_REVIEW,
                validation_criteria: &[],
            },
            ContextBudget::default(),
        )
        .expect("context fits")
    }

    #[tokio::test]
    async fn unavailable_is_passed_through_unchanged() {
        let model = ScriptedModel::new(vec![Err(AuthorModelError::Unavailable(
            "no model loaded".to_owned(),
        ))]);
        let result = extract_concepts(
            &model,
            &ConceptBatchInput {
                course_id: "course.rust",
                fragments: &[fragment("f-1", "ownership")],
            },
            CancellationToken::new(),
        )
        .await;
        match result {
            Err(AuthorError::Model(AuthorModelError::Unavailable(reason))) => {
                assert_eq!(reason, "no model loaded");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preempted_is_also_passed_through() {
        let model = ScriptedModel::new(vec![Err(AuthorModelError::Preempted("voice".to_owned()))]);
        let result = plan_curriculum(
            &model,
            &ConceptGraph {
                course_id: "course.rust".to_owned(),
                revision: 1,
                built_from: Vec::new(),
                concepts: Vec::new(),
            },
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            result,
            Err(AuthorError::Model(AuthorModelError::Preempted(_)))
        ));
    }

    #[tokio::test]
    async fn a_fenced_reply_is_parsed_end_to_end() {
        let model = ScriptedModel::new(vec![Ok(
            "```json\n{\"concepts\":[{\"id\":\"rust.ownership\",\"name\":\"Ownership\",\"description\":\"One owner.\",\"objectives\":[\"explain moves\"],\"prerequisites\":{},\"fragment_ids\":[\"f-1\"],\"difficulty\":2,\"validator\":\"exact_key\"}]}\n```"
                .to_owned(),
        )]);
        let parsed = extract_concepts(
            &model,
            &ConceptBatchInput {
                course_id: "course.rust",
                fragments: &[fragment("f-1", "ownership")],
            },
            CancellationToken::new(),
        )
        .await
        .expect("concepts parse");
        assert_eq!(parsed.value.concepts.len(), 1);
        assert_eq!(parsed.output_hash.len(), 64);
        assert_eq!(model.calls(), 1);
    }

    #[tokio::test]
    async fn an_unknown_lesson_is_rejected_without_calling_the_model() {
        let model = ScriptedModel::new(Vec::new());
        let plan = CurriculumPlan {
            course_id: "course.rust".to_owned(),
            revision: 1,
            concept_graph_revision: 1,
            title: "Rust".to_owned(),
            domain: crate::courses::content::Domain::Rust,
            level: "intro".to_owned(),
            scope: "ownership".to_owned(),
            outcomes: Vec::new(),
            units: Vec::new(),
            conflicts: Vec::new(),
            status: crate::factory::contracts::PlanStatus::Proposed,
        };
        let ctx = review_context();
        let result = author_lesson(&model, &plan, "missing", &ctx, CancellationToken::new()).await;
        assert!(matches!(result, Err(AuthorError::InvalidRequest(_))));
        assert_eq!(model.calls(), 0);
    }

    #[tokio::test]
    async fn a_review_variant_is_a_single_step() {
        let model = ScriptedModel::new(vec![Ok(
            r#"{"id":"r.1","kind":"choice","family":"new.family","title":"Which binding is valid?","objective_ids":["rust.ownership"],"options":[{"id":"a","text":"x"},{"id":"b","text":"y"}],"key":{"choice":["a"]}}"#
                .to_owned(),
        )]);
        let ctx = review_context();
        let parsed = author_review_variant(&model, &objective(), &ctx, CancellationToken::new())
            .await
            .expect("step parses");
        assert_eq!(parsed.value.id, "r.1");
    }

    #[tokio::test]
    async fn a_context_with_the_wrong_schema_is_rejected_without_calling_the_model() {
        let model = ScriptedModel::new(Vec::new());
        let objective = objective();
        let ctx = context::build(
            &AuthoringContext {
                objective: &objective,
                prerequisites: &[],
                fragments: &[],
                related_lessons: &[],
                question_families: &[],
                learner_evidence: "",
                schema: prompts::SCHEMA_LESSON,
                validation_criteria: &[],
            },
            ContextBudget::default(),
        )
        .expect("context fits");
        let result =
            author_review_variant(&model, &objective, &ctx, CancellationToken::new()).await;
        assert!(matches!(result, Err(AuthorError::InvalidRequest(_))));
        assert_eq!(model.calls(), 0);
    }
}
