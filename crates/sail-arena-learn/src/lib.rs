//! Arena Learn: Arena's main product, a local learning loop (probe → plan → teach/check).
//!
//! Spec: `docs/SAIL_ARENA_LEARN.md`. Tracking issue: #215.
//!
//! **Clean-room.** This is a behavior-level reimplementation inspired by LearnHarness
//! (AGPL-3.0). No LearnHarness source, prompt text, UI assets or fixtures are copied here.
//!
//! Layout, mirroring the boundaries the spec asks for:
//! - [`core`]: contracts, validation and projections. Pure: no IO, no clock, no model.
//! - [`store`]: SQLite adapter for the append-only session event log.
//! - [`agent`]: role tools, argument healing, prompts and the turn runner over a [`core::ports::Model`].
//!
//! This crate is separate from `sail-arena` so the memory store used by Flow, Dialogue and
//! Core takes no model dependency.

pub mod agent;
pub mod core;
pub mod store;

pub use agent::{Engine, RoleModels, TurnError};
pub use core::artifacts::{Artifact, ArtifactId, ArtifactKind, ValidationStatus};
pub use core::events::{Envelope, LearningEvent};
pub use core::graph::{Chain, ChainId, Confidence, GraphSummary, PropId, Proposition};
pub use core::ports::{EventStore, Model, StoreError};
pub use core::practice::{
    PracticeAttempt, PracticeOutcome, PracticeView, ProblemSpec, ProblemView,
};
pub use core::view::{SessionView, project_view};
pub use store::SqliteEventStore;
