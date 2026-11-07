//! Course Factory: turns owner-provided books and notes into source-grounded, validated
//! course packages, in small resumable background jobs. ADR and policy:
//! `docs/SAIL_ARENA_LEARN_COURSE_FACTORY.md`.
//!
//! Ownership of the submodules (see the ADR):
//! - [`contracts`]: shared types and ports (M0). Change only through the lead.
//!
//! Planned, each with its own tables (`learn_factory_<area>_*`) in the Arena Learn database:
//! `sources` + `ingest` (registry, PDF/EPUB/Markdown/text → fragments),
//! `author` (bounded contexts, prompts, model-output parsing),
//! `validate` (schema, grounding, copyright, subject validators, sandboxed exercise tests),
//! `jobs` (priority queue over Continuity, admission, preemption) and
//! `registry` (immutable published package revisions served by `CourseEngine`).
//!
//! Nothing here runs during a learner's lesson: published packages are plain
//! `CoursePackage`s, graded by `courses` without any model call.
pub mod author;
pub mod contracts;
pub mod ingest;
pub mod jobs;
pub mod mind_author;
pub mod registry;
pub mod service;
pub mod settings;
pub mod sources;
pub mod store;
pub mod validate;
