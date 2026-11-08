//! HTTPS client for Moodle's REST web service and token sign-in. **Owner: Codex.**
//! See *Codex details* in the handoff. Implements [`crate::contracts::MoodleApi`].

pub mod auth;
pub mod parse;
pub mod rest;

pub use auth::{AuthError, launch_url, new_passport, parse_launch_redirect};
pub use rest::RestClient;
