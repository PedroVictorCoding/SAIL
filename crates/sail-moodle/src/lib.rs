//! SAIL Moodle: a read-only connector to the owner's school Moodle.
//!
//! It signs in with a Moodle mobile web-service token (SSO sites hand one out through the
//! mobile launch flow), keeps a local snapshot of every enrolled course, reports what
//! changed (new files, moved deadlines, announcements, grades), mirrors course files, and
//! extracts dated items such as exams and tests with a local model whose every claim must
//! be grounded in a verbatim quote.
//!
//! Nothing is ever written to Moodle, and Moodle data never leaves the host: the only
//! outbound requests go to the configured Moodle site, and the model is SAIL's local one.
//! See `docs/SAIL_MOODLE.md`.
pub mod client;
pub mod contracts;
pub mod extract;
pub mod store;
pub mod sync;

pub use contracts::*;
