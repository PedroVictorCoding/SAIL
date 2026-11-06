//! Interactive Courses (#386): authored course packages, deterministic graders, an
//! append-only evidence log and a server-side review gate. Spec:
//! `docs/SAIL_ARENA_LEARN_COURSES.md`.
//!
//! Separate from the conversational loop in [`crate::agent`]. Nothing here calls a model,
//! and nothing here writes to the knowledge graph: competence evidence is not a claim
//! about the world.
pub mod content;
pub mod engine;
pub mod evidence;
pub mod grade;
pub mod store;

pub use engine::{AttemptRequest, CourseEngine, CourseError, NoSandbox, RustEvaluator};
pub use evidence::Context;
pub use store::CourseStore;
