//! Deterministic assembly of model proposals into the server-owned course structures.
//!
//! The model proposes; the server decides. This module:
//! - merges `ConceptProposal`s from many bounded batches into one [`ConceptGraph`], dropping
//!   fragment ids that were not in that batch's prompt;
//! - resolves those ids to [`SourceAnchor`]s and derives `built_from`;
//! - promotes a prerequisite edge from `Proposed` to `Supported` only when a cited source
//!   fragment actually uses the prerequisite later in reading order;
//! - rejects cycles;
//! - validates a [`CurriculumProposal`] against the graph and produces a `Proposed` plan;
//! - applies the owner's graph edits, returning a new revision.
//!
//! Everything here is pure over the supplied fragments and structs. No IO, no clock, no
//! model call, and no `unwrap` on model input.

use crate::courses::content::Domain;
use crate::factory::contracts::{
    Concept, ConceptGraph, ConceptId, ConceptProposal, ContentHash, CurriculumPlan,
    CurriculumProposal, EdgeStatus, FragmentId, FragmentLookup, LessonPlanKind, ObjectiveId,
    ObjectiveSpec, PlanStatus, PrerequisiteEdge, SourceAnchor, SourceAnchorSet, SourceFragment,
    SourceId, SourceLocation, ValidatorKind,
};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

/// Most concepts one graph may hold. The extractor batches well below this; the cap keeps a
/// hostile or runaway reply from growing the graph without bound.
pub const MAX_CONCEPTS: usize = 512;

/// Most anchors kept per concept.
pub const MAX_ANCHORS_PER_CONCEPT: usize = 64;

/// Most anchors attached to one prerequisite edge.
pub const MAX_EDGE_ANCHORS: usize = 8;

/// Most objectives kept per concept.
pub const MAX_OBJECTIVES_PER_CONCEPT: usize = 32;

/// Most units one curriculum plan may hold.
pub const MAX_UNITS: usize = 64;

/// Most lessons one unit may hold.
pub const MAX_LESSONS_PER_UNIT: usize = 64;

/// Most objectives one lesson may name.
pub const MAX_OBJECTIVES_PER_LESSON: usize = 32;

/// Distinct cycles reported before the report stops. All cycles still reject the graph.
pub const MAX_REPORTED_CYCLES: usize = 16;

/// Ordinary lessons are 3-8 active minutes, as the spec requires.
pub const MIN_LESSON_MINUTES: u32 = 3;

/// Upper bound for an ordinary lesson.
pub const MAX_LESSON_MINUTES: u32 = 8;

/// Upper bound for labs, assessments and capstones.
pub const MAX_OTHER_MINUTES: u32 = 600;

/// One bounded concept-extraction batch: the fragment ids that were in its prompt and the
/// reply the model produced.
pub struct ConceptBatch {
    pub prompt_fragment_ids: Vec<FragmentId>,
    pub proposal: ConceptProposal,
}

/// What assembly discarded or strengthened. The owner sees this in the inspector; it is not
/// model-controlled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssemblyReport {
    /// Fragment ids the model cited that were not in the batch prompt or did not resolve.
    pub dropped_fragment_ids: Vec<FragmentId>,
    /// Prerequisite concept ids that no concept in the graph defines.
    pub dropped_prerequisites: Vec<ConceptId>,
    /// Concepts merged because two batches proposed the same id.
    pub merged_by_id: usize,
    /// Concepts merged because two ids had the same normalised name.
    pub merged_by_name: usize,
    pub supported_edges: usize,
    pub proposed_edges: usize,
}

/// Why a graph or plan could not be assembled.
#[derive(Debug, thiserror::Error)]
pub enum AssembleError {
    #[error("concept `{concept_id}`: {reason}")]
    InvalidConcept {
        concept_id: ConceptId,
        reason: String,
    },
    #[error("invalid curriculum: {0}")]
    InvalidCurriculum(String),
    #[error("prerequisite cycles: {0}")]
    Cycles(String),
    #[error("invalid owner edit: {0}")]
    InvalidEdit(String),
    #[error("proposal too large: {0}")]
    TooLarge(String),
    /// A resolved anchor names a source revision the lookup cannot describe. Without the
    /// source revision's own content hash the graph could not be invalidated safely later,
    /// so assembly fails closed instead of recording a fragment hash as a source hash.
    #[error("source `{source_id}` revision {revision} is not resolvable: {reason}")]
    UnresolvedSource {
        source_id: SourceId,
        revision: u64,
        reason: &'static str,
    },
}

/// A concept while batches are still being merged.
struct PendingConcept {
    id: ConceptId,
    name: String,
    description: String,
    objectives: Vec<String>,
    prerequisites: BTreeMap<ConceptId, String>,
    anchors: Vec<SourceAnchor>,
    difficulty: u8,
    validator: ValidatorKind,
}

/// Merges model proposals into a concept graph at `revision`.
///
/// Only ids in the corresponding batch's prompt are resolved. Concepts with the same id are
/// merged; concepts with the same normalised name are merged and prerequisite references are
/// rewritten to the survivor. Edges that point at a concept nobody defined are dropped and
/// reported. A cycle rejects the whole graph.
pub fn assemble_concept_graph(
    course_id: &str,
    revision: u64,
    batches: &[ConceptBatch],
    fragments: &dyn FragmentLookup,
) -> Result<(ConceptGraph, AssemblyReport), AssembleError> {
    let mut report = AssemblyReport::default();
    let mut order: Vec<ConceptId> = Vec::new();
    let mut by_id: BTreeMap<ConceptId, PendingConcept> = BTreeMap::new();

    for batch in batches {
        let allowed: BTreeSet<&str> = batch
            .prompt_fragment_ids
            .iter()
            .map(String::as_str)
            .collect();
        for proposed in &batch.proposal.concepts {
            let pending = PendingConcept {
                id: proposed.id.clone(),
                name: proposed.name.clone(),
                description: proposed.description.clone(),
                objectives: proposed.objectives.clone(),
                prerequisites: proposed.prerequisites.clone(),
                anchors: resolve_anchors(&proposed.fragment_ids, &allowed, fragments, &mut report),
                difficulty: proposed.difficulty,
                validator: proposed.validator,
            };
            if pending.id.trim().is_empty() {
                return Err(AssembleError::InvalidConcept {
                    concept_id: pending.id,
                    reason: "empty concept id".to_owned(),
                });
            }
            if let Some(existing) = by_id.get_mut(&pending.id) {
                merge_pending(existing, pending);
                report.merged_by_id += 1;
            } else {
                order.push(pending.id.clone());
                by_id.insert(pending.id.clone(), pending);
            }
        }
    }
    if by_id.len() > MAX_CONCEPTS {
        return Err(AssembleError::TooLarge(format!(
            "{} concepts exceeds the {MAX_CONCEPTS} concept cap",
            by_id.len()
        )));
    }

    // Deduplicate by normalised name, rewriting references to the survivor.
    let mut by_name: BTreeMap<String, ConceptId> = BTreeMap::new();
    let mut aliases: BTreeMap<ConceptId, ConceptId> = BTreeMap::new();
    let mut final_order: Vec<ConceptId> = Vec::new();
    for id in order {
        let Some(pending) = by_id.remove(&id) else {
            continue;
        };
        let normalized = normalize_name(&pending.name);
        if !normalized.is_empty()
            && let Some(survivor) = by_name.get(&normalized).cloned()
        {
            if let Some(existing) = by_id.get_mut(&survivor) {
                merge_pending(existing, pending);
            }
            aliases.insert(id, survivor);
            report.merged_by_name += 1;
            continue;
        }
        if !normalized.is_empty() {
            by_name.insert(normalized, id.clone());
        }
        final_order.push(id.clone());
        by_id.insert(id, pending);
    }
    let aliases = resolve_aliases(&aliases);
    for pending in by_id.values_mut() {
        let id = pending.id.clone();
        let prerequisites = std::mem::take(&mut pending.prerequisites);
        pending.prerequisites = prerequisites
            .into_iter()
            .filter_map(|(requires, rationale)| {
                let requires = resolve_alias(&aliases, &requires);
                (requires != id).then_some((requires, rationale))
            })
            .collect();
    }

    let known: BTreeSet<ConceptId> = final_order.iter().cloned().collect();
    let mut concepts: Vec<Concept> = Vec::with_capacity(final_order.len());
    let mut meta: BTreeMap<ConceptId, (String, Vec<SourceAnchor>)> = BTreeMap::new();
    for id in &final_order {
        let pending = by_id.get(id).ok_or_else(|| AssembleError::InvalidConcept {
            concept_id: id.clone(),
            reason: "concept disappeared while assembling".to_owned(),
        })?;
        if pending.name.trim().is_empty() {
            return Err(AssembleError::InvalidConcept {
                concept_id: id.clone(),
                reason: "empty concept name".to_owned(),
            });
        }
        if !(1..=5).contains(&pending.difficulty) {
            return Err(AssembleError::InvalidConcept {
                concept_id: id.clone(),
                reason: format!("difficulty {} is outside 1..=5", pending.difficulty),
            });
        }
        meta.insert(id.clone(), (pending.name.clone(), pending.anchors.clone()));
        concepts.push(Concept {
            id: id.clone(),
            name: pending.name.trim().to_owned(),
            description: pending.description.trim().to_owned(),
            objectives: build_objectives(id, &pending.objectives),
            prerequisites: Vec::new(),
            anchors: pending.anchors.clone(),
            difficulty: pending.difficulty,
            exercise_families: Vec::new(),
            validator: pending.validator,
        });
    }

    for concept in &mut concepts {
        let Some(pending) = by_id.get(&concept.id) else {
            continue;
        };
        let mut edges = Vec::new();
        for (requires, rationale) in &pending.prerequisites {
            if !known.contains(requires) {
                report.dropped_prerequisites.push(requires.clone());
                continue;
            }
            let Some((prerequisite_name, prerequisite_anchors)) = meta.get(requires) else {
                report.dropped_prerequisites.push(requires.clone());
                continue;
            };
            let status = if edge_is_supported(
                &pending.anchors,
                prerequisite_anchors,
                prerequisite_name,
                fragments,
            ) {
                report.supported_edges += 1;
                EdgeStatus::Supported
            } else {
                report.proposed_edges += 1;
                EdgeStatus::Proposed
            };
            let rationale = if rationale.trim().is_empty() {
                "No rationale supplied.".to_owned()
            } else {
                rationale.trim().to_owned()
            };
            edges.push(PrerequisiteEdge {
                requires: requires.clone(),
                status,
                rationale,
                anchors: mentioning_anchors(&pending.anchors, prerequisite_name, fragments),
            });
        }
        concept.prerequisites = edges;
    }

    let graph = ConceptGraph {
        course_id: course_id.to_owned(),
        revision,
        built_from: built_from(&concepts, fragments)?,
        concepts,
    };
    let cycles = detect_cycles(&graph.concepts);
    if !cycles.is_empty() {
        return Err(AssembleError::Cycles(format_cycles(&cycles)));
    }
    Ok((graph, report))
}

/// Validates a curriculum proposal against the graph and returns a `Proposed` plan.
///
/// Rules enforced here: every named objective exists, unit and lesson ids are unique, lesson
/// minutes respect the kind's bounds, unit objectives are covered by the unit's lessons, and
/// every prerequisite is taught strictly before the concept that needs it.
pub fn assemble_curriculum_plan(
    graph: &ConceptGraph,
    proposal: CurriculumProposal,
    course_id: &str,
    revision: u64,
    domain: Domain,
) -> Result<CurriculumPlan, AssembleError> {
    if graph.course_id != course_id {
        return Err(AssembleError::InvalidCurriculum(
            "concept graph belongs to a different course".to_owned(),
        ));
    }
    if proposal.title.trim().is_empty() {
        return Err(AssembleError::InvalidCurriculum(
            "course title is empty".to_owned(),
        ));
    }
    if proposal.units.len() > MAX_UNITS {
        return Err(AssembleError::InvalidCurriculum(format!(
            "{} units exceeds the {MAX_UNITS} unit cap",
            proposal.units.len()
        )));
    }

    let mut known_objectives: BTreeSet<&str> = BTreeSet::new();
    for concept in &graph.concepts {
        for objective in &concept.objectives {
            known_objectives.insert(objective.id.as_str());
        }
    }
    let mut unit_ids: BTreeSet<&str> = BTreeSet::new();
    let mut lesson_ids: BTreeSet<&str> = BTreeSet::new();
    let mut schedule: BTreeMap<&str, usize> = BTreeMap::new();
    let mut lesson_index = 0usize;

    for unit in &proposal.units {
        if unit.id.trim().is_empty() || !unit_ids.insert(unit.id.as_str()) {
            return Err(AssembleError::InvalidCurriculum(format!(
                "empty or duplicate unit id `{}`",
                unit.id
            )));
        }
        if unit.lessons.len() > MAX_LESSONS_PER_UNIT {
            return Err(AssembleError::InvalidCurriculum(format!(
                "unit `{}` has more than {MAX_LESSONS_PER_UNIT} lessons",
                unit.id
            )));
        }
        let mut unit_objectives: BTreeSet<&str> = BTreeSet::new();
        for objective in &unit.objective_ids {
            if !known_objectives.contains(objective.as_str()) {
                return Err(AssembleError::InvalidCurriculum(format!(
                    "unit `{}` names unknown objective `{objective}`",
                    unit.id
                )));
            }
            unit_objectives.insert(objective.as_str());
        }
        for lesson in &unit.lessons {
            if lesson.id.trim().is_empty() || !lesson_ids.insert(lesson.id.as_str()) {
                return Err(AssembleError::InvalidCurriculum(format!(
                    "empty or duplicate lesson id `{}`",
                    lesson.id
                )));
            }
            if lesson.objective_ids.is_empty() {
                return Err(AssembleError::InvalidCurriculum(format!(
                    "lesson `{}` names no objective",
                    lesson.id
                )));
            }
            if lesson.objective_ids.len() > MAX_OBJECTIVES_PER_LESSON {
                return Err(AssembleError::InvalidCurriculum(format!(
                    "lesson `{}` names more than {MAX_OBJECTIVES_PER_LESSON} objectives",
                    lesson.id
                )));
            }
            for objective in &lesson.objective_ids {
                if !known_objectives.contains(objective.as_str()) {
                    return Err(AssembleError::InvalidCurriculum(format!(
                        "lesson `{}` names unknown objective `{objective}`",
                        lesson.id
                    )));
                }
                if !unit_objectives.contains(objective.as_str()) {
                    return Err(AssembleError::InvalidCurriculum(format!(
                        "lesson `{}` names objective `{objective}` that its unit does not",
                        lesson.id
                    )));
                }
                schedule.entry(objective.as_str()).or_insert(lesson_index);
            }
            if !minutes_in_bounds(lesson.kind, lesson.minutes) {
                return Err(AssembleError::InvalidCurriculum(format!(
                    "lesson `{}` has {} minutes, outside the bounds for {:?}",
                    lesson.id, lesson.minutes, lesson.kind
                )));
            }
            lesson_index += 1;
        }
        let covered: BTreeSet<&str> = unit
            .lessons
            .iter()
            .flat_map(|lesson| lesson.objective_ids.iter().map(String::as_str))
            .collect();
        if let Some(missing) = unit
            .objective_ids
            .iter()
            .find(|objective| !covered.contains(objective.as_str()))
        {
            return Err(AssembleError::InvalidCurriculum(format!(
                "unit `{}` lists objective `{missing}` but no lesson teaches it",
                unit.id
            )));
        }
    }

    for concept in &graph.concepts {
        let dependent_positions: Vec<usize> = concept
            .objectives
            .iter()
            .filter_map(|objective| schedule.get(objective.id.as_str()).copied())
            .collect();
        let Some(&dependent_first) = dependent_positions.iter().min() else {
            continue;
        };
        for edge in concept
            .prerequisites
            .iter()
            .filter(|edge| edge.status != EdgeStatus::OwnerRejected)
        {
            let Some(prerequisite) = graph
                .concepts
                .iter()
                .find(|candidate| candidate.id == edge.requires)
            else {
                return Err(AssembleError::InvalidCurriculum(format!(
                    "concept `{}` requires unknown concept `{}`",
                    concept.id, edge.requires
                )));
            };
            let prerequisite_positions: Vec<usize> = prerequisite
                .objectives
                .iter()
                .filter_map(|objective| schedule.get(objective.id.as_str()).copied())
                .collect();
            let Some(&prerequisite_first) = prerequisite_positions.iter().min() else {
                return Err(AssembleError::InvalidCurriculum(format!(
                    "prerequisite `{}` is not scheduled before `{}`",
                    prerequisite.id, concept.id
                )));
            };
            if prerequisite_first >= dependent_first {
                return Err(AssembleError::InvalidCurriculum(format!(
                    "`{}` is taught before its prerequisite `{}`",
                    concept.id, prerequisite.id
                )));
            }
        }
    }

    Ok(CurriculumPlan {
        course_id: course_id.to_owned(),
        revision,
        concept_graph_revision: graph.revision,
        title: proposal.title.trim().to_owned(),
        domain,
        level: proposal.level.trim().to_owned(),
        scope: proposal.scope.trim().to_owned(),
        outcomes: proposal.outcomes,
        units: proposal.units,
        conflicts: proposal.conflicts,
        status: PlanStatus::Proposed,
    })
}

/// An owner edit to a concept graph. Each edit returns a new revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphEdit {
    Rename {
        concept_id: ConceptId,
        name: String,
    },
    /// Merge every concept in `from` into `into`; references are rewritten.
    Merge {
        from: Vec<ConceptId>,
        into: ConceptId,
    },
    Remove {
        concept_id: ConceptId,
    },
    /// Replace the concept order with exactly these ids.
    Reorder {
        order: Vec<ConceptId>,
    },
    ConfirmEdge {
        concept_id: ConceptId,
        requires: ConceptId,
    },
    RejectEdge {
        concept_id: ConceptId,
        requires: ConceptId,
    },
}

/// Applies owner edits and returns a new graph revision.
///
/// Unknown ids, self-merges and a `Reorder` that is not a permutation of the current ids are
/// errors. A cycle introduced by the edits is rejected; the owner keeps the old revision.
pub fn apply_owner_edits(
    graph: &ConceptGraph,
    edits: &[GraphEdit],
) -> Result<ConceptGraph, AssembleError> {
    let mut next = graph.clone();
    next.revision = graph.revision.saturating_add(1);

    for edit in edits {
        match edit {
            GraphEdit::Rename { concept_id, name } => {
                if name.trim().is_empty() {
                    return Err(AssembleError::InvalidEdit(
                        "a concept name may not be empty".to_owned(),
                    ));
                }
                let concept = concept_mut(&mut next, concept_id)?;
                concept.name = name.trim().to_owned();
            }
            GraphEdit::Merge { from, into } => {
                merge_concepts(&mut next, from, into)?;
            }
            GraphEdit::Remove { concept_id } => {
                let Some(position) = next
                    .concepts
                    .iter()
                    .position(|concept| &concept.id == concept_id)
                else {
                    return Err(AssembleError::InvalidEdit(format!(
                        "unknown concept `{concept_id}`"
                    )));
                };
                next.concepts.remove(position);
                for concept in &mut next.concepts {
                    concept
                        .prerequisites
                        .retain(|edge| &edge.requires != concept_id);
                }
            }
            GraphEdit::Reorder { order } => {
                reorder_concepts(&mut next, order)?;
            }
            GraphEdit::ConfirmEdge {
                concept_id,
                requires,
            } => set_edge_status(&mut next, concept_id, requires, EdgeStatus::OwnerConfirmed)?,
            GraphEdit::RejectEdge {
                concept_id,
                requires,
            } => set_edge_status(&mut next, concept_id, requires, EdgeStatus::OwnerRejected)?,
        }
    }

    let cycles = detect_cycles(&next.concepts);
    if !cycles.is_empty() {
        return Err(AssembleError::Cycles(format_cycles(&cycles)));
    }
    Ok(next)
}

fn merge_concepts(
    graph: &mut ConceptGraph,
    from: &[ConceptId],
    into: &ConceptId,
) -> Result<(), AssembleError> {
    let mut from = from.to_vec();
    from.sort();
    from.dedup();
    if from.iter().any(|id| id == into) {
        return Err(AssembleError::InvalidEdit(
            "a concept cannot be merged into itself".to_owned(),
        ));
    }
    if !graph.concepts.iter().any(|concept| &concept.id == into) {
        return Err(AssembleError::InvalidEdit(format!(
            "unknown concept `{into}`"
        )));
    }
    for id in &from {
        if !graph.concepts.iter().any(|concept| &concept.id == id) {
            return Err(AssembleError::InvalidEdit(format!(
                "unknown concept `{id}`"
            )));
        }
    }

    for id in &from {
        let Some(position) = graph.concepts.iter().position(|concept| &concept.id == id) else {
            continue;
        };
        let removed = graph.concepts.remove(position);
        let Some(target_position) = graph
            .concepts
            .iter()
            .position(|concept| &concept.id == into)
        else {
            return Err(AssembleError::InvalidEdit(format!(
                "unknown concept `{into}`"
            )));
        };
        let target = &mut graph.concepts[target_position];
        for objective in removed.objectives {
            if !target
                .objectives
                .iter()
                .any(|existing| existing.id == objective.id)
            {
                target.objectives.push(objective);
            }
        }
        for anchor in removed.anchors {
            if !target
                .anchors
                .iter()
                .any(|existing| existing.fragment_id == anchor.fragment_id)
            {
                target.anchors.push(anchor);
            }
        }
        for family in removed.exercise_families {
            if !target.exercise_families.contains(&family) {
                target.exercise_families.push(family);
            }
        }
        for mut edge in removed.prerequisites {
            if &edge.requires == id {
                edge.requires = into.clone();
            }
            if &edge.requires == into {
                continue;
            }
            target.prerequisites.push(edge);
        }
    }

    for concept in &mut graph.concepts {
        for edge in &mut concept.prerequisites {
            if from.contains(&edge.requires) {
                edge.requires = into.clone();
            }
        }
        concept
            .prerequisites
            .retain(|edge| edge.requires != concept.id);
        dedupe_edges(concept);
    }
    Ok(())
}

fn reorder_concepts(graph: &mut ConceptGraph, order: &[ConceptId]) -> Result<(), AssembleError> {
    let current: BTreeSet<&str> = graph
        .concepts
        .iter()
        .map(|concept| concept.id.as_str())
        .collect();
    let offered: BTreeSet<&str> = order.iter().map(String::as_str).collect();
    if current != offered || order.len() != graph.concepts.len() {
        return Err(AssembleError::InvalidEdit(
            "reorder must list every concept id exactly once".to_owned(),
        ));
    }
    let mut by_id: BTreeMap<ConceptId, Concept> = std::mem::take(&mut graph.concepts)
        .into_iter()
        .map(|concept| (concept.id.clone(), concept))
        .collect();
    graph.concepts = order.iter().filter_map(|id| by_id.remove(id)).collect();
    Ok(())
}

fn set_edge_status(
    graph: &mut ConceptGraph,
    concept_id: &str,
    requires: &str,
    status: EdgeStatus,
) -> Result<(), AssembleError> {
    let concept = concept_mut(graph, concept_id)?;
    let Some(edge) = concept
        .prerequisites
        .iter_mut()
        .find(|edge| edge.requires == requires)
    else {
        return Err(AssembleError::InvalidEdit(format!(
            "`{concept_id}` has no prerequisite `{requires}`"
        )));
    };
    edge.status = status;
    Ok(())
}

fn concept_mut<'a>(
    graph: &'a mut ConceptGraph,
    concept_id: &str,
) -> Result<&'a mut Concept, AssembleError> {
    graph
        .concepts
        .iter_mut()
        .find(|concept| concept.id == concept_id)
        .ok_or_else(|| AssembleError::InvalidEdit(format!("unknown concept `{concept_id}`")))
}

fn dedupe_edges(concept: &mut Concept) {
    let mut unique: BTreeMap<ConceptId, PrerequisiteEdge> = BTreeMap::new();
    for edge in std::mem::take(&mut concept.prerequisites) {
        match unique.entry(edge.requires.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(edge);
            }
            Entry::Occupied(mut slot) => {
                if status_rank(edge.status) > status_rank(slot.get().status) {
                    slot.insert(edge);
                }
            }
        }
    }
    concept.prerequisites = unique.into_values().collect();
}

fn status_rank(status: EdgeStatus) -> u8 {
    match status {
        EdgeStatus::OwnerRejected => 0,
        EdgeStatus::Proposed => 1,
        EdgeStatus::Supported => 2,
        EdgeStatus::OwnerConfirmed => 3,
    }
}

fn minutes_in_bounds(kind: LessonPlanKind, minutes: u32) -> bool {
    match kind {
        LessonPlanKind::Lesson => (MIN_LESSON_MINUTES..=MAX_LESSON_MINUTES).contains(&minutes),
        LessonPlanKind::Lab
        | LessonPlanKind::MidCourseAssessment
        | LessonPlanKind::FinalAssessment
        | LessonPlanKind::Capstone => (1..=MAX_OTHER_MINUTES).contains(&minutes),
    }
}

fn resolve_anchors(
    ids: &[FragmentId],
    allowed: &BTreeSet<&str>,
    lookup: &dyn FragmentLookup,
    report: &mut AssemblyReport,
) -> Vec<SourceAnchor> {
    let mut anchors = Vec::new();
    let mut seen: BTreeSet<FragmentId> = BTreeSet::new();
    for id in ids {
        if !allowed.contains(id.as_str()) {
            report.dropped_fragment_ids.push(id.clone());
            continue;
        }
        let Some(fragment) = lookup.fragment(id) else {
            report.dropped_fragment_ids.push(id.clone());
            continue;
        };
        if !seen.insert(fragment.fragment_id.clone()) {
            continue;
        }
        anchors.push(SourceAnchor {
            source_id: fragment.source_id,
            source_revision: fragment.source_revision,
            fragment_id: fragment.fragment_id,
            content_hash: fragment.content_hash,
        });
        if anchors.len() >= MAX_ANCHORS_PER_CONCEPT {
            break;
        }
    }
    anchors
}

fn merge_pending(into: &mut PendingConcept, from: PendingConcept) {
    for objective in from.objectives {
        if !into.objectives.contains(&objective) {
            into.objectives.push(objective);
        }
    }
    for (requires, rationale) in from.prerequisites {
        into.prerequisites.entry(requires).or_insert(rationale);
    }
    for anchor in from.anchors {
        if !into
            .anchors
            .iter()
            .any(|existing| existing.fragment_id == anchor.fragment_id)
        {
            into.anchors.push(anchor);
        }
    }
    into.anchors.truncate(MAX_ANCHORS_PER_CONCEPT);
}

fn resolve_aliases(aliases: &BTreeMap<ConceptId, ConceptId>) -> BTreeMap<ConceptId, ConceptId> {
    aliases
        .iter()
        .map(|(from, to)| (from.clone(), resolve_alias(aliases, to)))
        .collect()
}

fn resolve_alias(aliases: &BTreeMap<ConceptId, ConceptId>, id: &str) -> ConceptId {
    let mut current = id.to_owned();
    let mut hops = 0usize;
    while let Some(next) = aliases.get(&current) {
        if next == &current || hops >= aliases.len() {
            break;
        }
        current = next.clone();
        hops += 1;
    }
    current
}

fn build_objectives(concept_id: &str, statements: &[String]) -> Vec<ObjectiveSpec> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut objectives = Vec::new();
    for statement in statements {
        let statement = statement.trim();
        if statement.is_empty() || !seen.insert(statement.to_owned()) {
            continue;
        }
        objectives.push(ObjectiveSpec {
            id: format!("{concept_id}.objective.{}", objectives.len() + 1),
            statement: statement.to_owned(),
        });
        if objectives.len() >= MAX_OBJECTIVES_PER_CONCEPT {
            break;
        }
    }
    objectives
}

fn mentioning_anchors(
    anchors: &[SourceAnchor],
    prerequisite_name: &str,
    lookup: &dyn FragmentLookup,
) -> Vec<SourceAnchor> {
    let name = normalize_name(prerequisite_name);
    if name.is_empty() {
        return Vec::new();
    }
    anchors
        .iter()
        .filter(|anchor| {
            lookup
                .fragment(&anchor.fragment_id)
                .is_some_and(|fragment| mentions_name(&fragment.text, &name))
        })
        .take(MAX_EDGE_ANCHORS)
        .cloned()
        .collect()
}

fn edge_is_supported(
    dependent_anchors: &[SourceAnchor],
    prerequisite_anchors: &[SourceAnchor],
    prerequisite_name: &str,
    lookup: &dyn FragmentLookup,
) -> bool {
    let name = normalize_name(prerequisite_name);
    if name.is_empty() {
        return false;
    }
    let Some(prerequisite_first) = prerequisite_anchors
        .iter()
        .filter_map(|anchor| lookup.fragment(&anchor.fragment_id))
        .min_by_key(reading_order)
    else {
        return false;
    };
    let prerequisite_order = reading_order(&prerequisite_first);
    dependent_anchors.iter().any(|anchor| {
        lookup
            .fragment(&anchor.fragment_id)
            .is_some_and(|fragment| {
                reading_order(&fragment) > prerequisite_order
                    && mentions_name(&fragment.text, &name)
            })
    })
}

/// Whether `text` contains `normalized_name` as a whole word sequence.
///
/// Whole-word matching matters: a prerequisite named `move` must not be "mentioned" by a
/// sentence that only contains `remove`. Both sides are normalised; padding with spaces makes
/// the ends of the text word boundaries too.
fn mentions_name(text: &str, normalized_name: &str) -> bool {
    if normalized_name.is_empty() {
        return false;
    }
    let haystack = format!(" {} ", normalize_name(text));
    let needle = format!(" {normalized_name} ");
    haystack.contains(&needle)
}

/// Reading order within a source: line ranges, then pages, then EPUB href/anchor order.
/// Comparing different sources is not meaningful, so `source_id` is the first key. The
/// contract's `SourceLocation::Epub` has no spine index, so href order is the deterministic
/// approximation available here; a factory parser that knows the spine can extend this.
fn reading_order(fragment: &SourceFragment) -> (String, u8, u64, String) {
    let (class, ordinal, tail) = match &fragment.location {
        SourceLocation::Lines { first, .. } => (0u8, u64::from(*first), String::new()),
        SourceLocation::Page { physical, .. } => (1u8, u64::from(*physical), String::new()),
        SourceLocation::PageRange { physical_first, .. } => {
            (1u8, u64::from(*physical_first), String::new())
        }
        SourceLocation::Epub { href, anchor } => (2u8, 0, format!("{href}#{anchor}")),
    };
    (fragment.source_id.clone(), class, ordinal, tail)
}

/// The source revisions a graph was built from, keyed by `(source_id, revision)`.
///
/// `SourceAnchorSet.content_hash` is the **source revision's** content hash — the same value
/// [`crate::factory::sources::SourceRegistry::affected_by`] reports and the grounding
/// validator compares against `EducationalSource::content_hash`. Storing a fragment hash
/// would make later invalidation unable to match the graph, so the source record is resolved
/// here and a missing or moved source is an error rather than a fabricated hash.
fn built_from(
    concepts: &[Concept],
    lookup: &dyn FragmentLookup,
) -> Result<Vec<SourceAnchorSet>, AssembleError> {
    let mut revisions: BTreeMap<(SourceId, u64), ContentHash> = BTreeMap::new();
    for concept in concepts {
        let anchors = concept.anchors.iter().chain(
            concept
                .prerequisites
                .iter()
                .flat_map(|edge| edge.anchors.iter()),
        );
        for anchor in anchors {
            let key = (anchor.source_id.clone(), anchor.source_revision);
            if revisions.contains_key(&key) {
                continue;
            }
            let source = lookup.source(&anchor.source_id).ok_or_else(|| {
                AssembleError::UnresolvedSource {
                    source_id: anchor.source_id.clone(),
                    revision: anchor.source_revision,
                    reason: "the source record is not available",
                }
            })?;
            if source.revision != anchor.source_revision {
                return Err(AssembleError::UnresolvedSource {
                    source_id: anchor.source_id.clone(),
                    revision: anchor.source_revision,
                    reason: "the lookup returned a different current revision",
                });
            }
            revisions.insert(key, source.content_hash);
        }
    }
    Ok(revisions
        .into_iter()
        .map(
            |((source_id, source_revision), content_hash)| SourceAnchorSet {
                source_id,
                source_revision,
                content_hash,
            },
        )
        .collect())
}

fn normalize_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    let mut pending_space = false;
    for character in name.chars() {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            pending_space = false;
            normalized.extend(character.to_lowercase());
        } else {
            pending_space = true;
        }
    }
    normalized
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    Grey,
    Black,
}

fn detect_cycles(concepts: &[Concept]) -> Vec<Vec<ConceptId>> {
    let known: BTreeSet<&str> = concepts.iter().map(|concept| concept.id.as_str()).collect();
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for concept in concepts {
        let mut requires: Vec<&str> = concept
            .prerequisites
            .iter()
            .filter(|edge| {
                edge.status != EdgeStatus::OwnerRejected && known.contains(edge.requires.as_str())
            })
            .map(|edge| edge.requires.as_str())
            .collect();
        requires.sort_unstable();
        requires.dedup();
        edges.insert(concept.id.as_str(), requires);
    }
    let mut state: BTreeMap<&str, Visit> = BTreeMap::new();
    let mut stack: Vec<&str> = Vec::new();
    let mut cycles: Vec<Vec<ConceptId>> = Vec::new();
    for node in edges.keys().copied() {
        if !state.contains_key(node) {
            depth_first(node, &edges, &mut state, &mut stack, &mut cycles);
        }
        if cycles.len() >= MAX_REPORTED_CYCLES {
            break;
        }
    }
    cycles
}

fn depth_first<'a>(
    node: &'a str,
    edges: &BTreeMap<&'a str, Vec<&'a str>>,
    state: &mut BTreeMap<&'a str, Visit>,
    stack: &mut Vec<&'a str>,
    cycles: &mut Vec<Vec<ConceptId>>,
) {
    state.insert(node, Visit::Grey);
    stack.push(node);
    if let Some(next_nodes) = edges.get(node) {
        for &next in next_nodes {
            if cycles.len() >= MAX_REPORTED_CYCLES {
                break;
            }
            match state.get(next).copied() {
                Some(Visit::Grey) => {
                    if let Some(position) = stack.iter().position(|candidate| *candidate == next) {
                        let mut cycle: Vec<ConceptId> = stack[position..]
                            .iter()
                            .map(|candidate| (*candidate).to_owned())
                            .collect();
                        cycle.push(next.to_owned());
                        canonicalize_cycle(&mut cycle);
                        if !cycles.contains(&cycle) {
                            cycles.push(cycle);
                        }
                    }
                }
                Some(Visit::Black) => {}
                None => depth_first(next, edges, state, stack, cycles),
            }
        }
    }
    stack.pop();
    state.insert(node, Visit::Black);
}

fn canonicalize_cycle(cycle: &mut Vec<ConceptId>) {
    if cycle.len() < 2 {
        return;
    }
    cycle.pop();
    if let Some((position, _)) = cycle.iter().enumerate().min_by(|(_, a), (_, b)| a.cmp(b)) {
        cycle.rotate_left(position);
    }
    if let Some(first) = cycle.first().cloned() {
        cycle.push(first);
    }
}

fn format_cycles(cycles: &[Vec<ConceptId>]) -> String {
    cycles
        .iter()
        .map(|cycle| cycle.join(" -> "))
        .collect::<Vec<String>>()
        .join("; ")
}

/// Objective id helper used by tests and callers that need the deterministic scheme.
pub fn objective_id(concept_id: &str, ordinal: usize) -> ObjectiveId {
    format!("{concept_id}.objective.{ordinal}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::contracts::{
        EducationalSource, FragmentKind, IngestionStatus, LessonPlan, ProposedConcept,
        QualityStatus, SourceRights, SourceType, UnitPlan,
    };

    struct FakeLookup {
        fragments: BTreeMap<FragmentId, SourceFragment>,
        sources: BTreeMap<SourceId, EducationalSource>,
    }

    impl FakeLookup {
        fn new(fragments: Vec<SourceFragment>) -> Self {
            let mut sources = BTreeMap::new();
            for fragment in &fragments {
                sources
                    .entry(fragment.source_id.clone())
                    .or_insert_with(|| EducationalSource {
                        source_id: fragment.source_id.clone(),
                        title: "Synthetic source".to_owned(),
                        authors: Vec::new(),
                        source_type: SourceType::Markdown,
                        content_hash: format!("source-hash-{}", fragment.source_id),
                        revision: fragment.source_revision,
                        language: "en".to_owned(),
                        access_scope: "learning:test".to_owned(),
                        rights: SourceRights::Restricted,
                        status: IngestionStatus::Indexed,
                        imported_ms: 0,
                    });
            }
            Self {
                fragments: fragments
                    .into_iter()
                    .map(|fragment| (fragment.fragment_id.clone(), fragment))
                    .collect(),
                sources,
            }
        }

        fn without_sources(fragments: Vec<SourceFragment>) -> Self {
            Self {
                fragments: fragments
                    .into_iter()
                    .map(|fragment| (fragment.fragment_id.clone(), fragment))
                    .collect(),
                sources: BTreeMap::new(),
            }
        }
    }

    impl FragmentLookup for FakeLookup {
        fn source(&self, id: &str) -> Option<EducationalSource> {
            self.sources.get(id).cloned()
        }
        fn fragment(&self, id: &str) -> Option<SourceFragment> {
            self.fragments.get(id).cloned()
        }
    }

    fn fragment(id: &str, text: &str, first_line: u32) -> SourceFragment {
        SourceFragment {
            fragment_id: id.to_owned(),
            source_id: "src-rust".to_owned(),
            source_revision: 1,
            kind: FragmentKind::Prose,
            chapter_path: vec!["Ownership".to_owned()],
            location: SourceLocation::Lines {
                first: first_line,
                last: first_line + 1,
            },
            text: text.to_owned(),
            content_hash: format!("hash-{id}"),
            quality: QualityStatus::Clean,
        }
    }

    fn proposed(
        id: &str,
        name: &str,
        fragment_ids: &[&str],
        prerequisites: &[(&str, &str)],
    ) -> ProposedConcept {
        ProposedConcept {
            id: id.to_owned(),
            name: name.to_owned(),
            description: format!("{name} in one sentence."),
            objectives: vec![format!("The learner can apply {name}.")],
            prerequisites: prerequisites
                .iter()
                .map(|(requires, why)| ((*requires).to_owned(), (*why).to_owned()))
                .collect(),
            fragment_ids: fragment_ids.iter().map(|id| (*id).to_owned()).collect(),
            difficulty: 2,
            validator: ValidatorKind::ExactKey,
        }
    }

    fn batch(prompt_fragment_ids: &[&str], concepts: Vec<ProposedConcept>) -> ConceptBatch {
        ConceptBatch {
            prompt_fragment_ids: prompt_fragment_ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            proposal: ConceptProposal { concepts },
        }
    }

    fn lesson_plan(id: &str, objectives: &[&str], minutes: u32) -> LessonPlan {
        LessonPlan {
            id: id.to_owned(),
            title: format!("Lesson {id}"),
            objective_ids: objectives.iter().map(|o| (*o).to_owned()).collect(),
            minutes,
            kind: LessonPlanKind::Lesson,
        }
    }

    #[test]
    fn fragments_outside_the_batch_prompt_are_dropped() {
        let lookup = FakeLookup::new(vec![
            fragment("f-1", "ownership", 1),
            fragment("f-9", "borrowing", 2),
        ]);
        let graph = assemble_concept_graph(
            "course.rust",
            1,
            &[batch(
                &["f-1"],
                vec![proposed(
                    "rust.ownership",
                    "Ownership",
                    &["f-1", "f-9"],
                    &[],
                )],
            )],
            &lookup,
        );
        let (graph, report) = graph.expect("graph assembles");
        assert_eq!(graph.concepts[0].anchors.len(), 1);
        assert_eq!(graph.concepts[0].anchors[0].fragment_id, "f-1");
        // The anchor binds the fragment, but `built_from` must name the *source* revision:
        // `SourceRegistry::affected_by` and the grounding validator use source hashes.
        assert_eq!(graph.concepts[0].anchors[0].content_hash, "hash-f-1");
        assert!(report.dropped_fragment_ids.contains(&"f-9".to_owned()));
        assert_eq!(graph.built_from.len(), 1);
        assert_eq!(graph.built_from[0].content_hash, "source-hash-src-rust");
    }

    #[test]
    fn a_source_the_lookup_cannot_describe_is_a_typed_error() {
        let lookup = FakeLookup::without_sources(vec![fragment("f-1", "ownership", 1)]);
        let result = assemble_concept_graph(
            "course.rust",
            1,
            &[batch(
                &["f-1"],
                vec![proposed("rust.ownership", "Ownership", &["f-1"], &[])],
            )],
            &lookup,
        );
        assert!(matches!(
            result,
            Err(AssembleError::UnresolvedSource {
                source_id,
                revision: 1,
                ..
            }) if source_id == "src-rust"
        ));
    }

    #[test]
    fn duplicate_ids_and_names_are_merged_and_references_rewired() {
        let lookup = FakeLookup::new(vec![
            fragment("f-a", "ownership", 1),
            fragment("f-b", "borrowing builds on ownership", 3),
        ]);
        let graph = assemble_concept_graph(
            "course.rust",
            1,
            &[batch(
                &["f-a", "f-b"],
                vec![
                    proposed("c.one", "Ownership", &["f-a"], &[]),
                    proposed("c.two", " ownership ", &["f-a"], &[]),
                    proposed("c.three", "Borrowing", &["f-b"], &[("c.two", "needs it")]),
                ],
            )],
            &lookup,
        );
        let (graph, report) = graph.expect("graph assembles");
        assert_eq!(graph.concepts.len(), 2);
        assert!(report.merged_by_name >= 1);
        let borrowing = graph
            .concepts
            .iter()
            .find(|concept| concept.name == "Borrowing")
            .expect("borrowing concept");
        assert_eq!(borrowing.prerequisites[0].requires, "c.one");
    }

    #[test]
    fn a_cycle_rejects_the_graph() {
        let lookup = FakeLookup::new(vec![
            fragment("f-a", "alpha", 1),
            fragment("f-b", "beta", 2),
        ]);
        let result = assemble_concept_graph(
            "course.rust",
            1,
            &[batch(
                &["f-a", "f-b"],
                vec![
                    proposed("a", "Alpha", &["f-a"], &[("b", "after b")]),
                    proposed("b", "Beta", &["f-b"], &[("a", "after a")]),
                ],
            )],
            &lookup,
        );
        match result {
            Err(AssembleError::Cycles(message)) => assert!(message.contains("a -> b")),
            other => panic!("expected a cycle error, got {other:?}"),
        }
    }

    #[test]
    fn chapter_order_alone_leaves_an_edge_proposed() {
        let lookup = FakeLookup::new(vec![
            fragment("f-a", "Ownership means one owner.", 1),
            fragment("f-b", "A borrow reads through a reference.", 10),
        ]);
        let (graph, report) = assemble_concept_graph(
            "course.rust",
            1,
            &[batch(
                &["f-a", "f-b"],
                vec![
                    proposed("a", "Ownership", &["f-a"], &[]),
                    proposed("b", "Borrowing", &["f-b"], &[("a", "needed first")]),
                ],
            )],
            &lookup,
        )
        .expect("graph assembles");
        let borrowing = graph
            .concepts
            .iter()
            .find(|concept| concept.id == "b")
            .expect("borrowing");
        assert_eq!(borrowing.prerequisites[0].status, EdgeStatus::Proposed);
        assert_eq!(report.proposed_edges, 1);
    }

    #[test]
    fn a_later_mention_of_the_prerequisite_supports_the_edge() {
        let lookup = FakeLookup::new(vec![
            fragment("f-a", "Ownership means one owner.", 1),
            fragment(
                "f-b",
                "Borrowing checks ownership before lending a reference.",
                10,
            ),
        ]);
        let (graph, report) = assemble_concept_graph(
            "course.rust",
            1,
            &[batch(
                &["f-a", "f-b"],
                vec![
                    proposed("a", "Ownership", &["f-a"], &[]),
                    proposed("b", "Borrowing", &["f-b"], &[("a", "needed first")]),
                ],
            )],
            &lookup,
        )
        .expect("graph assembles");
        let borrowing = graph
            .concepts
            .iter()
            .find(|concept| concept.id == "b")
            .expect("borrowing");
        assert_eq!(borrowing.prerequisites[0].status, EdgeStatus::Supported);
        assert_eq!(report.supported_edges, 1);
        assert_eq!(borrowing.prerequisites[0].anchors.len(), 1);
    }

    #[test]
    fn a_longer_word_containing_the_name_does_not_support_the_edge() {
        let lookup = FakeLookup::new(vec![
            fragment("f-a", "Move semantics transfer a value.", 1),
            fragment("f-b", "Remove the old binding afterwards.", 10),
        ]);
        let (graph, _) = assemble_concept_graph(
            "course.rust",
            1,
            &[batch(
                &["f-a", "f-b"],
                vec![
                    proposed("a", "Move", &["f-a"], &[]),
                    proposed("b", "Removal", &["f-b"], &[("a", "needed first")]),
                ],
            )],
            &lookup,
        )
        .expect("graph assembles");
        let dependent = graph
            .concepts
            .iter()
            .find(|concept| concept.id == "b")
            .expect("dependent");
        assert_eq!(dependent.prerequisites[0].status, EdgeStatus::Proposed);
    }

    fn two_concept_graph() -> ConceptGraph {
        let lookup = FakeLookup::new(vec![
            fragment("f-a", "Ownership means one owner.", 1),
            fragment("f-b", "Borrowing checks ownership first.", 10),
        ]);
        let (graph, _) = assemble_concept_graph(
            "course.rust",
            1,
            &[batch(
                &["f-a", "f-b"],
                vec![
                    proposed("a", "Ownership", &["f-a"], &[]),
                    proposed("b", "Borrowing", &["f-b"], &[("a", "needed first")]),
                ],
            )],
            &lookup,
        )
        .expect("graph assembles");
        graph
    }

    fn proposal(title: &str, units: Vec<UnitPlan>) -> CurriculumProposal {
        CurriculumProposal {
            title: title.to_owned(),
            level: "intro".to_owned(),
            scope: "ownership".to_owned(),
            outcomes: vec!["explain moves".to_owned()],
            units,
            conflicts: Vec::new(),
        }
    }

    #[test]
    fn curriculum_respects_prerequisite_order() {
        let graph = two_concept_graph();
        let a = graph.concepts[0].objectives[0].id.clone();
        let b = graph.concepts[1].objectives[0].id.clone();
        let good = proposal(
            "Rust",
            vec![UnitPlan {
                id: "u1".to_owned(),
                title: "Unit".to_owned(),
                objective_ids: vec![a.clone(), b.clone()],
                lessons: vec![lesson_plan("l1", &[&a], 5), lesson_plan("l2", &[&b], 5)],
                review_checkpoint: true,
            }],
        );
        let plan =
            assemble_curriculum_plan(&graph, good, "course.rust", 1, Domain::Rust).expect("valid");
        assert_eq!(plan.status, PlanStatus::Proposed);
        assert_eq!(plan.concept_graph_revision, 1);

        let reversed = proposal(
            "Rust",
            vec![UnitPlan {
                id: "u1".to_owned(),
                title: "Unit".to_owned(),
                objective_ids: vec![a.clone(), b.clone()],
                lessons: vec![lesson_plan("l1", &[&b], 5), lesson_plan("l2", &[&a], 5)],
                review_checkpoint: true,
            }],
        );
        assert!(matches!(
            assemble_curriculum_plan(&graph, reversed, "course.rust", 1, Domain::Rust),
            Err(AssembleError::InvalidCurriculum(_))
        ));
    }

    #[test]
    fn curriculum_rejects_bad_minutes_and_unknown_objectives() {
        let graph = two_concept_graph();
        let a = graph.concepts[0].objectives[0].id.clone();
        let long = proposal(
            "Rust",
            vec![UnitPlan {
                id: "u1".to_owned(),
                title: "Unit".to_owned(),
                objective_ids: vec![a.clone()],
                lessons: vec![lesson_plan("l1", &[&a], 9)],
                review_checkpoint: false,
            }],
        );
        assert!(matches!(
            assemble_curriculum_plan(&graph, long, "course.rust", 1, Domain::Rust),
            Err(AssembleError::InvalidCurriculum(_))
        ));
        let unknown = proposal(
            "Rust",
            vec![UnitPlan {
                id: "u1".to_owned(),
                title: "Unit".to_owned(),
                objective_ids: vec!["nope".to_owned()],
                lessons: vec![lesson_plan("l1", &["nope"], 5)],
                review_checkpoint: false,
            }],
        );
        assert!(matches!(
            assemble_curriculum_plan(&graph, unknown, "course.rust", 1, Domain::Rust),
            Err(AssembleError::InvalidCurriculum(_))
        ));
    }

    #[test]
    fn owner_edits_return_a_new_revision_and_rewrite_references() {
        let graph = two_concept_graph();
        let owned = apply_owner_edits(
            &graph,
            &[
                GraphEdit::Rename {
                    concept_id: "a".to_owned(),
                    name: "Move semantics".to_owned(),
                },
                GraphEdit::ConfirmEdge {
                    concept_id: "b".to_owned(),
                    requires: "a".to_owned(),
                },
            ],
        )
        .expect("edits apply");
        assert_eq!(owned.revision, graph.revision + 1);
        assert_eq!(owned.concepts[0].name, "Move semantics");
        assert_eq!(
            owned.concepts[1].prerequisites[0].status,
            EdgeStatus::OwnerConfirmed
        );
    }

    #[test]
    fn merge_moves_objectives_and_edges_to_the_survivor() {
        let graph = two_concept_graph();
        let merged = apply_owner_edits(
            &graph,
            &[GraphEdit::Merge {
                from: vec!["b".to_owned()],
                into: "a".to_owned(),
            }],
        )
        .expect("merge applies");
        assert_eq!(merged.concepts.len(), 1);
        assert_eq!(merged.concepts[0].id, "a");
        assert_eq!(merged.concepts[0].objectives.len(), 2);
        assert!(merged.concepts[0].prerequisites.is_empty());
    }

    #[test]
    fn remove_drops_edges_to_the_removed_concept() {
        let graph = two_concept_graph();
        let removed = apply_owner_edits(
            &graph,
            &[GraphEdit::Remove {
                concept_id: "a".to_owned(),
            }],
        )
        .expect("remove applies");
        assert_eq!(removed.concepts.len(), 1);
        assert!(removed.concepts[0].prerequisites.is_empty());
    }

    #[test]
    fn reorder_requires_exactly_the_current_ids() {
        let graph = two_concept_graph();
        let reordered = apply_owner_edits(
            &graph,
            &[GraphEdit::Reorder {
                order: vec!["b".to_owned(), "a".to_owned()],
            }],
        )
        .expect("reorder applies");
        assert_eq!(reordered.concepts[0].id, "b");
        assert!(matches!(
            apply_owner_edits(
                &graph,
                &[GraphEdit::Reorder {
                    order: vec!["b".to_owned()],
                }]
            ),
            Err(AssembleError::InvalidEdit(_))
        ));
    }

    #[test]
    fn rejecting_an_edge_removes_it_from_plan_constraints() {
        let graph = two_concept_graph();
        let rejected = apply_owner_edits(
            &graph,
            &[GraphEdit::RejectEdge {
                concept_id: "b".to_owned(),
                requires: "a".to_owned(),
            }],
        )
        .expect("reject applies");
        assert_eq!(
            rejected.concepts[1].prerequisites[0].status,
            EdgeStatus::OwnerRejected
        );
        let a = rejected.concepts[0].objectives[0].id.clone();
        let b = rejected.concepts[1].objectives[0].id.clone();
        let out_of_order = proposal(
            "Rust",
            vec![UnitPlan {
                id: "u1".to_owned(),
                title: "Unit".to_owned(),
                objective_ids: vec![a.clone(), b.clone()],
                lessons: vec![lesson_plan("l1", &[&b], 5), lesson_plan("l2", &[&a], 5)],
                review_checkpoint: false,
            }],
        );
        assemble_curriculum_plan(&rejected, out_of_order, "course.rust", 2, Domain::Rust)
            .expect("rejected edge no longer constrains the plan");
    }
}
